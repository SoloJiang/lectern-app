//! Where HEAD stayed during a call (docs/design/usage.md「分支观察」): the
//! replay of a worktree's HEAD reflog records around the call's start and
//! result times. Pure: the store fetches the records.
//!
//! A record's write time is not exact. Its interval runs from the later of
//! the previous read's start and the line's own second, to the earlier of
//! its read's end and the line's second plus one. A record inside the call
//! is a switch the call made; one that crosses the result time makes the
//! call unsure; one that crosses the start time is counted only when it is
//! the one.

/// What HEAD points at.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Head {
    /// A branch, by its full reference name.
    Branch(String),
    /// A commit, no branch.
    Detached,
}

/// A HEAD reflog record that switched HEAD, with the interval its write
/// lies in (nanoseconds).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Switch {
    pub(crate) lo: i64,
    pub(crate) hi: i64,
    pub(crate) to: Head,
}

/// Where HEAD stayed: the heads it was on during the call, in order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Observed {
    pub(crate) stays: Vec<Stay>,
    /// False when the order of the records and the call could not be told
    /// (a switch crossing the result time, several crossing the start time,
    /// or a gap in the reading).
    pub(crate) certain: bool,
}

/// A head HEAD was on and a time at which it was.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Stay {
    pub(crate) head: Head,
    /// Nanoseconds, inside the call.
    pub(crate) at: i64,
}

impl Observed {
    /// The head HEAD ended on.
    pub(crate) fn end(&self) -> Option<&Stay> {
        self.stays.last()
    }

    /// HEAD was on more than one head.
    pub(crate) fn switched(&self) -> bool {
        self.stays.len() > 1
    }
}

/// Replays `switches` (in the order they were read) from `baseline`, the
/// head HEAD was on before the first of them, for a call that began at
/// `start` and whose result came at `end` (nanoseconds). `gap` says reading
/// the reflog failed somewhere in the call.
pub(crate) fn replay(
    baseline: &Head,
    switches: &[Switch],
    start: i64,
    end: i64,
    gap: bool,
) -> Observed {
    let mut state = baseline.clone();
    let mut certain = !gap;
    let mut crossing_start: Vec<&Switch> = Vec::new();
    let mut during: Vec<&Switch> = Vec::new();
    for s in switches {
        if s.lo > end {
            // After the result (the records are in the order they were read).
            break;
        }
        if s.hi >= end {
            certain = false;
            break;
        }
        if s.hi < start {
            state = s.to.clone();
        } else if s.lo <= start {
            crossing_start.push(s);
        } else {
            during.push(s);
        }
    }
    let mut stays = Vec::new();
    let mut push = |head: &Head, at: i64| {
        if stays.last().is_none_or(|s: &Stay| &s.head != head) {
            stays.push(Stay {
                head: head.clone(),
                at,
            });
        }
    };
    match crossing_start.len() {
        0 => push(&state, start),
        1 => {}
        _ => certain = false,
    }
    // The one switch across the start counts, the several do not: only
    // their last is the head HEAD went on from there.
    if let Some(last) = crossing_start.last() {
        push(&last.to, last.lo.max(start));
    }
    for s in during {
        push(&s.to, s.lo);
    }
    Observed { stays, certain }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b(name: &str) -> Head {
        Head::Branch(name.into())
    }

    fn sw(lo: i64, hi: i64, to: &str) -> Switch {
        Switch { lo, hi, to: b(to) }
    }

    #[test]
    fn replay_before_the_call_only_sets_the_state() {
        let o = replay(&b("a"), &[sw(1, 2, "b")], 10, 20, false);
        assert_eq!(o.stays.len(), 1);
        assert_eq!(o.stays[0].head, b("b"));
        assert!(o.certain && !o.switched());
    }

    #[test]
    fn replay_a_switch_inside_the_call_is_a_stay() {
        let o = replay(&b("a"), &[sw(12, 13, "b")], 10, 20, false);
        let heads: Vec<_> = o.stays.iter().map(|s| s.head.clone()).collect();
        assert_eq!(heads, [b("a"), b("b")]);
        assert!(o.certain && o.switched());
    }

    #[test]
    fn replay_a_switch_after_the_result_does_not_count() {
        let o = replay(&b("a"), &[sw(30, 31, "b")], 10, 20, false);
        assert_eq!(o.stays.len(), 1);
        assert!(o.certain);
    }

    #[test]
    fn replay_a_switch_across_the_result_makes_the_call_unsure() {
        let o = replay(&b("a"), &[sw(15, 25, "b")], 10, 20, false);
        assert!(!o.certain);
        assert_eq!(o.end().unwrap().head, b("a"));
    }

    #[test]
    fn replay_one_switch_across_the_start_counts_its_target() {
        let o = replay(&b("a"), &[sw(5, 12, "b")], 10, 20, false);
        let heads: Vec<_> = o.stays.iter().map(|s| s.head.clone()).collect();
        assert_eq!(heads, [b("b")]);
        assert!(o.certain);
    }

    #[test]
    fn replay_several_across_the_start_are_unsure() {
        let o = replay(&b("a"), &[sw(5, 12, "b"), sw(6, 13, "c")], 10, 20, false);
        assert!(!o.certain);
        assert_eq!(o.end().unwrap().head, b("c"));
    }

    #[test]
    fn replay_a_gap_makes_the_call_unsure() {
        let o = replay(&b("a"), &[], 10, 20, true);
        assert!(!o.certain);
        assert_eq!(o.stays.len(), 1);
    }

    #[test]
    fn replay_the_same_head_twice_in_a_row_is_one_stay() {
        let o = replay(&b("a"), &[sw(12, 13, "a")], 10, 20, false);
        assert_eq!(o.stays.len(), 1);
    }
}
