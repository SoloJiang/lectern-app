//! What the model hands to other plugins (docs/design/review_view.md「接口」
//! 「界面」): the open file in the reader (⇧⌘O), the focus for code
//! navigation (the files of the review, the file the agent touched, the
//! selected row), the session's usage, and Space for the context panel. All
//! of it goes through `Shared`'s handoff services; when the plugin
//! "review-view-handoff" is not there, none of it happens and the screen
//! works without it.

use std::sync::Arc;

use gpui::Context;
use lectern_logging::{debug, info};
use lectern_services::code_intel::{Focus, TextOrigin, TextRef};
use lectern_services::project::{ProjectKey, WorktreeKey};
use lectern_services::reader::{Anchor, ReadTarget};
use lectern_services::review::{ReviewTarget, RowsKey};
use lectern_services::session::Place;
use lectern_services::usage::{SessionUsage, UsageQuery, UsageSnapshot};
use lectern_services::workspace::WorkspaceCommand;

use crate::anchor;
use crate::diag;
use crate::highlight::Half;
use crate::items::{self, DisplayRow, Item};
use crate::model::{HandedOff, Missing, ReviewModel};

/// The `ID` of the context panel `workspace` shows on Space.
pub(crate) const CONTEXT_PANEL: &str = "context";

/// The row the selection stands for, and the text it shows.
pub(crate) struct Selected {
    pub text: TextRef,
    pub project: ProjectKey,
    pub byte: Option<u64>,
    pub line: u32,
}

impl ReviewModel {
    pub(crate) fn log_missing(&mut self, what: Missing) {
        let bit = what as u8;
        if self.handed_off.missing_logged & bit != 0 {
            return;
        }
        self.handed_off.missing_logged |= bit;
        info!("handoff unavailable"; id = self.id.0, what = what.name());
    }

    /// Whether the reader and the code navigation are there: the handoff
    /// buttons are enabled by it.
    pub(crate) fn can_hand_off(&self) -> bool {
        self.shared.has_handoff()
    }

    /// The worktree under review, for ordering the usage rows.
    pub(crate) fn focus_worktree(&self) -> Option<WorktreeKey> {
        self.focus_scope().and_then(|(_, worktree)| worktree)
    }

    /// The project and worktree the paths of this review belong to: a
    /// review of a worktree names them, a session's come from where the
    /// session was located. None until the session is.
    fn focus_scope(&self) -> Option<(ProjectKey, Option<WorktreeKey>)> {
        match &self.target {
            ReviewTarget::Outside { project, worktree }
            | ReviewTarget::Range {
                project, worktree, ..
            } => Some((project.clone(), Some(worktree.clone()))),
            ReviewTarget::Session(key) => {
                let summary = self.shared.session_summary(key)?;
                match summary.place {
                    Place::Located(location) => Some((location.project, location.worktree)),
                    _ => None,
                }
            }
        }
    }

    /// `Review` and `Touched` once the review is ready, and again when the
    /// snapshot's lists are other ones.
    pub(crate) fn sync_focus(&mut self) {
        if !self.is_ready() || !self.opened {
            return;
        }
        let Some((project, worktree)) = self.focus_scope() else {
            return;
        };
        let review = self.snapshot.focus_review.clone();
        let touched = self.snapshot.focus_touched.clone();
        let fresh = |sent: &Option<Arc<[Arc<str>]>>, now: &Arc<[Arc<str>]>| {
            !sent.as_ref().is_some_and(|sent| Arc::ptr_eq(sent, now))
        };
        let send_review = fresh(&self.handed_off.review, &review);
        let send_touched = fresh(&self.handed_off.touched, &touched);
        if !send_review && !send_touched {
            return;
        }
        let sent = self.shared.with_code_intel(|code_intel| {
            if send_review {
                code_intel.set_focus(&project, worktree.as_ref(), Focus::Review(review.clone()));
            }
            if send_touched {
                code_intel.set_focus(&project, worktree.as_ref(), Focus::Touched(touched.clone()));
            }
        });
        if sent.is_none() {
            self.log_missing(Missing::Focus);
            return;
        }
        if send_review {
            debug!("focus set"; id = self.id.0, kind = "review", paths = diag::count(review.len()));
            self.handed_off.review = Some(review);
        }
        if send_touched {
            debug!("focus set"; id = self.id.0, kind = "touched", paths = diag::count(touched.len()));
            self.handed_off.touched = Some(touched);
        }
    }

    /// The selected row, with the key of its diff.
    fn selected_row(&self) -> Option<(RowsKey, DisplayRow)> {
        let selected = self.selection.as_ref()?;
        let at = anchor::find(&self.items, selected)?;
        let (rows, ix) = match &self.items[at] {
            Item::Row { rows, ix, .. } => (rows.clone(), *ix as usize),
            // A hunk stands for its first row.
            Item::Hunk { rows, hunk, .. } => {
                let display = self.displays.get(rows)?;
                let ix = display.iter().position(|row| row.hunk == Some(*hunk))?;
                (rows.clone(), ix)
            }
            _ => return None,
        };
        let row = self.displays.get(&rows)?.get(ix)?.clone();
        Some((rows, row))
    }

    /// The text of the selected row for the reader and the code navigation:
    /// the new side of a row that has one, otherwise the old side (so an
    /// added or a context line is on the new side, a removed line on the
    /// old). The new side of a file's final diff is the working tree's
    /// content, every other side is a blob.
    pub(crate) fn selected_text(&self) -> Option<Selected> {
        let (rows_key, row) = self.selected_row()?;
        let (side, half) = match (row.right, row.left) {
            (Some(side), _) => (side, Half::New),
            (None, Some(side)) => (side, Half::Old),
            (None, None) => return None,
        };
        let rows = self.rows.get(&rows_key)?;
        let oid = match half {
            Half::Old => rows.old.oid?,
            Half::New => rows.new.oid?,
        };
        let file = items::rows_file(&self.snapshot, &rows_key)?;
        let path = match half {
            Half::Old => file.old.as_ref().or(file.new.as_ref()),
            Half::New => file.new.as_ref().or(file.old.as_ref()),
        }?;
        let path: Arc<str> = Arc::from(path.as_str()?);
        let origin = if half == Half::New && matches!(rows_key, RowsKey::Final { .. }) {
            TextOrigin::Worktree
        } else {
            TextOrigin::Object
        };
        let (project, worktree) = self.focus_scope()?;
        Some(Selected {
            text: TextRef {
                worktree,
                path,
                oid,
                origin,
            },
            project,
            byte: side.byte,
            line: side.line,
        })
    }

    /// `Current` follows the selected row. Clearing the selection sends
    /// nothing: the panel stays on what it showed.
    pub(crate) fn sync_current(&mut self) {
        if !self.opened {
            return;
        }
        let Some(selected) = self.selected_text() else {
            return;
        };
        if self.handed_off.current.as_ref() == Some(&selected.text) {
            return;
        }
        let worktree = selected.text.worktree.clone();
        let text = selected.text.clone();
        let sent = self.shared.with_code_intel(|code_intel| {
            code_intel.set_focus(
                &selected.project,
                worktree.as_ref(),
                Focus::Current(Some(text.clone())),
            );
        });
        if sent.is_none() {
            self.log_missing(Missing::Focus);
            return;
        }
        debug!("focus set"; id = self.id.0, kind = "current", paths = 1u32);
        self.handed_off.current = Some(selected.text);
        self.handed_off.current_sent = true;
    }

    /// The screen closes: the context panel has nothing of this review to
    /// follow any more.
    pub(crate) fn clear_current(&mut self) {
        if !self.handed_off.current_sent {
            return;
        }
        self.handed_off.current_sent = false;
        self.handed_off.current = None;
        let Some((project, worktree)) = self.focus_scope() else {
            return;
        };
        self.shared.with_code_intel(|code_intel| {
            code_intel.set_focus(&project, worktree.as_ref(), Focus::Current(None));
        });
    }

    /// ⇧⌘O and ⌘E: the selected row in the full file; with nothing selected
    /// the start of the current file.
    pub(crate) fn open_file(&mut self) {
        let Some(target) = self.read_target() else {
            if !self.can_hand_off() {
                self.log_missing(Missing::OpenFile);
            }
            return;
        };
        if self
            .shared
            .with_reader(|reader| reader.open(target))
            .is_none()
        {
            self.log_missing(Missing::OpenFile);
        }
    }

    /// The start of a file in the reader (the open button of a title).
    pub(crate) fn open_file_of(&mut self, file: &lectern_services::review::FileKey) {
        let target = file
            .new
            .as_ref()
            .and_then(|path| path.as_str())
            .zip(self.focus_scope())
            .map(|(path, (project, worktree))| ReadTarget {
                project,
                worktree,
                path: Arc::from(path),
                origin: TextOrigin::Worktree,
                expect: None,
                at: Anchor::Start,
            });
        let opened =
            target.and_then(|target| self.shared.with_reader(|reader| reader.open(target)));
        if opened.is_none() {
            self.log_missing(Missing::OpenFile);
        }
    }

    fn read_target(&self) -> Option<ReadTarget> {
        if let Some(selected) = self.selected_text() {
            let at = match selected.byte {
                Some(byte) => Anchor::Byte {
                    byte,
                    line: selected.line,
                    column: 0,
                },
                None => Anchor::Line {
                    line: selected.line,
                    column: 0,
                },
            };
            return Some(ReadTarget {
                project: selected.project,
                worktree: selected.text.worktree,
                path: selected.text.path,
                origin: selected.text.origin,
                expect: Some(selected.text.oid),
                at,
            });
        }
        let file = self.current_file()?;
        let path: Arc<str> = Arc::from(file.new.as_ref()?.as_str()?);
        let (project, worktree) = self.focus_scope()?;
        Some(ReadTarget {
            project,
            worktree,
            path,
            origin: TextOrigin::Worktree,
            expect: None,
            at: Anchor::Start,
        })
    }

    /// Space: the selected row becomes the context panel's focus, and the
    /// panel opens.
    pub(crate) fn open_context(&mut self) {
        let Some(selected) = self.selected_text() else {
            return;
        };
        let worktree = selected.text.worktree.clone();
        let text = selected.text.clone();
        let sent = self.shared.with_code_intel(|code_intel| {
            code_intel.set_focus(
                &selected.project,
                worktree.as_ref(),
                Focus::Current(Some(text.clone())),
            );
        });
        if sent.is_none() {
            self.log_missing(Missing::Focus);
            return;
        }
        self.handed_off.current = Some(selected.text);
        self.handed_off.current_sent = true;
        self.shared
            .dispatch(WorkspaceCommand::OpenPanel(lectern_kernel::Name::Borrowed(
                CONTEXT_PANEL,
            )));
    }

    // ----- usage -----------------------------------------------------------

    /// Starts watching the session's usage, when this is a session and the
    /// usage service is there.
    pub(crate) fn attach_usage(&mut self) {
        let ReviewTarget::Session(session) = &self.target else {
            return;
        };
        if self.usage_interest.is_some() || !self.opened {
            return;
        }
        let query = UsageQuery::Session {
            session: session.clone(),
        };
        let Some(interest) = self.shared.with_usage(|usage| usage.watch(query.clone())) else {
            self.log_missing(Missing::Usage);
            return;
        };
        self.usage_interest = Some(interest);
        self.usage_query = Some(query);
        self.take_usage();
    }

    /// Reads the usage snapshot `Usage` holds; false when it is not a
    /// session's (or nothing yet).
    fn take_usage(&mut self) -> bool {
        let Some(query) = &self.usage_query else {
            return false;
        };
        let snapshot = self
            .shared
            .with_usage(|usage| usage.snapshot(query))
            .flatten();
        match snapshot {
            Some(snapshot) if matches!(&*snapshot, UsageSnapshot::Session(_)) => {
                self.usage = Some(snapshot);
                true
            }
            _ => false,
        }
    }

    /// `UsageEvent::Changed` for this screen's query.
    pub(crate) fn read_usage(&mut self, cx: &mut Context<ReviewModel>) {
        if self.take_usage() {
            if let Some(UsageSnapshot::Session(usage)) = self.usage.as_deref() {
                debug!(
                    "usage applied";
                    id = self.id.0,
                    rows = diag::count(usage.rows.len()),
                    turns = diag::count(usage.turns.len())
                );
            }
            // The brief grew or shrank by a line per branch.
            self.list.remeasure_items(0..1);
            cx.notify();
        }
    }

    /// The session's usage, if it was read.
    pub(crate) fn session_usage(&self) -> Option<&SessionUsage> {
        match self.usage.as_deref()? {
            UsageSnapshot::Session(usage) => Some(usage),
            _ => None,
        }
    }

    /// The handoff services arrived or left: everything that depends on
    /// them is done again, or let go.
    pub(crate) fn handoff_changed(&mut self, cx: &mut Context<ReviewModel>) {
        self.handed_off = HandedOff::default();
        if self.can_hand_off() {
            self.usage_interest = None;
            self.attach_usage();
            self.sync_focus();
            self.sync_current();
            let held: Vec<_> = self
                .rows
                .iter()
                .map(|(key, rows)| (key.clone(), rows.clone()))
                .collect();
            for (key, rows) in held {
                self.after_rows(&key, &rows);
            }
        } else {
            self.usage_interest = None;
            self.usage_query = None;
            self.usage = None;
            self.shares.clear();
        }
        self.list.remeasure_items(0..1);
        cx.notify();
    }

    /// The tokens of a turn in this session's usage; the sub-agents' turns
    /// are not in it.
    pub(crate) fn turn_tokens(&self, turn: &lectern_services::review::TurnEntry) -> Option<u64> {
        if turn.at.session != 0 {
            return None;
        }
        let usage = self.session_usage()?;
        usage
            .turns
            .iter()
            .find(|(id, _)| *id == turn.id)
            .map(|(_, tokens)| tokens.total())
    }
}
