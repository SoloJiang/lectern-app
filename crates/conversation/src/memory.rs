//! What the page remembers of a session for as long as Lectern runs
//! (docs/design/conversation.md「数据结构」的「草稿」): the reply box's text and
//! the page (Messages or Terminal) the session was last on. In memory only; a
//! prompt is never written to disk. At most 16 sessions, the least recently
//! used one goes first; a draft over 256 KiB is not kept.

use std::sync::Arc;

use lectern_services::session::SessionKey;

use crate::view::Page;

/// How many sessions are remembered.
pub(crate) const MAX_SESSIONS: usize = 16;

/// The longest draft that is kept, in bytes.
pub(crate) const MAX_DRAFT: usize = 256 * 1024;

#[derive(Clone, PartialEq, Eq, Debug)]
struct Remembered {
    session: SessionKey,
    draft: Option<Arc<str>>,
    page: Option<Page>,
}

/// The remembered sessions, the most recently used last. Pure data: the
/// callers hold the lock around it.
#[derive(Default, Debug)]
pub(crate) struct Memory {
    sessions: Vec<Remembered>,
}

impl Memory {
    pub(crate) fn new() -> Memory {
        Memory::default()
    }

    /// The session's entry, made the most recently used; a new one is made
    /// when there is none, and the least recently used one goes when that
    /// is the seventeenth.
    fn touch(&mut self, session: &SessionKey) -> &mut Remembered {
        match self.sessions.iter().position(|r| r.session == *session) {
            Some(at) => {
                let found = self.sessions.remove(at);
                self.sessions.push(found);
            }
            None => {
                if self.sessions.len() == MAX_SESSIONS {
                    self.sessions.remove(0);
                }
                self.sessions.push(Remembered {
                    session: session.clone(),
                    draft: None,
                    page: None,
                });
            }
        }
        let last = self.sessions.len() - 1;
        &mut self.sessions[last]
    }

    /// Drops the entry of a session that holds nothing any more.
    fn tidy(&mut self, session: &SessionKey) {
        self.sessions
            .retain(|r| r.session != *session || r.draft.is_some() || r.page.is_some());
    }

    /// The draft of a session, if one is kept. Counts as a use.
    pub(crate) fn draft(&mut self, session: &SessionKey) -> Option<Arc<str>> {
        if !self.sessions.iter().any(|r| r.session == *session) {
            return None;
        }
        self.touch(session).draft.clone()
    }

    /// Keeps `text` as the session's draft. Text that is empty, or longer
    /// than [`MAX_DRAFT`], is not kept (and replaces an earlier draft: the
    /// older text is not what the box held last).
    pub(crate) fn set_draft(&mut self, session: &SessionKey, text: &str) {
        let keep = !text.is_empty() && text.len() <= MAX_DRAFT;
        if !keep && !self.sessions.iter().any(|r| r.session == *session) {
            return;
        }
        self.touch(session).draft = keep.then(|| Arc::from(text));
        self.tidy(session);
    }

    /// The page the session was last on. Counts as a use.
    pub(crate) fn page(&mut self, session: &SessionKey) -> Option<Page> {
        if !self.sessions.iter().any(|r| r.session == *session) {
            return None;
        }
        self.touch(session).page
    }

    pub(crate) fn set_page(&mut self, session: &SessionKey, page: Page) {
        self.touch(session).page = Some(page);
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.sessions.len()
    }
}
