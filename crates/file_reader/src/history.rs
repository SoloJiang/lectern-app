//! The back and forward history (docs/design/file_reader.md「范围」: 前进、
//! 后退). Memory only, at most [`HISTORY_MAX`] positions each way. An entry
//! is a `ReadTarget` that records where the reading was: the path, the
//! origin, the OID of the content seen (`expect`) and the position (`at`).
//! Never the content (docs/design/file_reader.md「定位」).

use std::collections::VecDeque;

use lectern_services::reader::ReadTarget;

/// How many positions the history keeps in each direction.
pub const HISTORY_MAX: usize = 100;

/// Positions left behind (the last is the nearest) and positions gone back
/// from (the last is the nearest).
#[derive(Clone, Default, Debug)]
pub struct History {
    back: VecDeque<ReadTarget>,
    forward: VecDeque<ReadTarget>,
}

impl History {
    /// The reading moved to a new place of its own (an open, not back or
    /// forward): `left` is where it was. What was ahead is gone.
    pub fn visit(&mut self, left: Option<ReadTarget>) {
        self.forward.clear();
        if let Some(left) = left {
            push(&mut self.back, left);
        }
    }

    /// Where back goes to, if anywhere.
    pub fn peek_back(&self) -> Option<&ReadTarget> {
        self.back.back()
    }

    /// Where forward goes to, if anywhere.
    pub fn peek_forward(&self) -> Option<&ReadTarget> {
        self.forward.back()
    }

    /// The reading went back to `peek_back`'s position: `left` is where it
    /// was, and forward goes there now.
    pub fn went_back(&mut self, left: Option<ReadTarget>) {
        self.back.pop_back();
        if let Some(left) = left {
            push(&mut self.forward, left);
        }
    }

    /// The reading went forward to `peek_forward`'s position: `left` is
    /// where it was, and back goes there now.
    pub fn went_forward(&mut self, left: Option<ReadTarget>) {
        self.forward.pop_back();
        if let Some(left) = left {
            push(&mut self.back, left);
        }
    }

    pub fn can_back(&self) -> bool {
        !self.back.is_empty()
    }

    pub fn can_forward(&self) -> bool {
        !self.forward.is_empty()
    }

    /// The positions behind and ahead.
    pub fn len(&self) -> (usize, usize) {
        (self.back.len(), self.forward.len())
    }

    pub fn is_empty(&self) -> bool {
        self.back.is_empty() && self.forward.is_empty()
    }
}

fn push(list: &mut VecDeque<ReadTarget>, target: ReadTarget) {
    list.push_back(target);
    while list.len() > HISTORY_MAX {
        list.pop_front();
    }
}
