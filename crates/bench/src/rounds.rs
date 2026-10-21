//! Rounds of a time-boxed query whose budget includes completion
//! (ADR 0043, docs/design/bench.md「时间盒截断的轮次」). A round that ends complete is a sample of its
//! measured times. A round the time box cut is a failing sample of every ID
//! of the round: it is charged [`Round::charge`], over every budget of such
//! a benchmark, so the median rule decides (more than half the rounds cut
//! is over budget). Any other incomplete round is an error.

use std::time::Duration;

use crate::{Measured, Value};

/// How one round ended, from the query's public results and the rig's
/// store.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Round {
    /// From the query to its end (the event of its last batch).
    pub elapsed: Duration,
    /// The query's time box.
    pub time_box: Duration,
    /// None when complete, else what was left out: (unsearched, outside).
    pub partial: Option<(u64, u64)>,
    /// Units the rig's store lacks against what the fixture wrote (None:
    /// not known).
    pub missing: Option<u64>,
    /// Evictions of the rig's store (None: not known).
    pub evictions: Option<u64>,
}

/// How a round counts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    Complete,
    /// The time box cut the check outside the search's scope: everything in
    /// the scope was searched, the query ran to its time box, and the store
    /// holds every unit the fixture wrote, none evicted. Its samples are
    /// charged.
    Cut,
    /// Not complete, and not shown to be the time box: why.
    Incomplete(String),
}

impl Round {
    /// The public results cannot say why a file outside the scope was left
    /// out; the time box is taken as the cause only on this evidence: no
    /// file left unsearched, the end at or past the time box, no unit
    /// missing, no eviction. Anything unknown is not evidence.
    pub fn verdict(&self) -> Verdict {
        let Some((unsearched, outside)) = self.partial else {
            return Verdict::Complete;
        };
        let why = if unsearched > 0 {
            format!("{unsearched} files unsearched")
        } else if outside == 0 {
            "partial with nothing left out".to_owned()
        } else if self.elapsed < self.time_box {
            format!(
                "{outside} outside, ended after {:?}, before its {:?} time box",
                self.elapsed, self.time_box
            )
        } else if self.missing != Some(0) {
            format!("{outside} outside, units missing: {:?}", self.missing)
        } else if self.evictions != Some(0) {
            format!("{outside} outside, evictions: {:?}", self.evictions)
        } else {
            return Verdict::Cut;
        };
        Verdict::Incomplete(why)
    }

    /// The value every sample of a cut round takes: its time plus one more
    /// time box. Its own end comes just past the time box, inside a budget
    /// that leaves 0.1 s to draw (references: 1.9 s of 2 s), so its own time
    /// would pass; a complete answer needs at least another query.
    pub fn charge(&self) -> Duration {
        self.elapsed + self.time_box
    }
}

/// The cut rounds of one run of a benchmark, warm-up rounds included.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Cuts {
    /// Rounds cut.
    pub rounds: u64,
    /// The most files left outside by one cut round.
    pub max_outside: u64,
    /// The last cut round's diagnosis.
    pub last: Option<String>,
}

impl Cuts {
    /// Judges `round`, with `diagnosis` describing the rig: `Ok(None)` when
    /// complete (its samples are its measured times), `Ok(Some(charge))`
    /// when cut (recorded here; every sample of the round takes the
    /// charge), `Err(why)` otherwise.
    pub fn judge(&mut self, round: &Round, diagnosis: &str) -> Result<Option<Duration>, String> {
        match round.verdict() {
            Verdict::Complete => Ok(None),
            Verdict::Cut => {
                let outside = round.partial.map_or(0, |(_, o)| o);
                self.rounds += 1;
                self.max_outside = self.max_outside.max(outside);
                self.last = Some(format!(
                    "{} rounds cut by the time box, the last with {outside} outside; {diagnosis}",
                    self.rounds
                ));
                Ok(Some(round.charge()))
            }
            Verdict::Incomplete(why) => Err(why),
        }
    }

    /// The note for the benchmark's results: the last cut, if any.
    pub fn note(&self) -> Option<String> {
        self.last.clone()
    }

    /// `<prefix>/partial_rounds` and `<prefix>/outside` (record only), with
    /// the note.
    pub fn measured(&self, prefix: &str) -> Vec<Measured> {
        vec![
            Measured::new(
                format!("{prefix}/partial_rounds"),
                Value::Count(self.rounds),
            )
            .with_note(self.note()),
            Measured::new(format!("{prefix}/outside"), Value::Count(self.max_outside))
                .with_note(self.note()),
        ]
    }
}
