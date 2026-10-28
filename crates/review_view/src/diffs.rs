//! What the model does with the diffs it holds beyond the list of items
//! (docs/design/review_view.md「高亮」「双栏」「折叠展开」): the highlight
//! tasks and the spans that come back from them, the object registrations
//! with `file_reader`, the unified / side-by-side switch and expanding a
//! fold. Main thread; the only blocking work (the parse and the query) is in
//! [`highlight::run`], on the executor.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use gpui::Context;
use lectern_logging::debug;
use lectern_services::review::{FileRows, RowsKey};
use lectern_services::workspace::WorkspaceCommand;
use lectern_syntax::WindowSpans;

use crate::anchor;
use crate::highlight::{self, Half, SideSpans, SpanKey, Work};
use crate::items::{self, DEFAULT_CONTEXT, DiffMode, DisplayRow, Item, ItemKey};
use crate::model::{Inflight, Missing, ReviewModel};

impl ReviewModel {
    /// The path of the file a diff belongs to, as text; None when it is not
    /// UTF-8 (such a file is shown as plain text).
    pub(crate) fn rows_path(&self, key: &RowsKey) -> Option<String> {
        let file = items::rows_file(&self.snapshot, key)?;
        file.new
            .as_ref()
            .or(file.old.as_ref())?
            .as_str()
            .map(str::to_owned)
    }

    /// Prepared rows were stored: registers their two sides with the reader
    /// and starts highlighting them.
    pub(crate) fn after_rows(&mut self, key: &RowsKey, rows: &Arc<FileRows>) {
        self.share_rows(key, rows);
        self.highlight_rows(key, rows);
    }

    /// `Reader::share_object` for each side that has text and an OID; the
    /// registrations live as long as the rows entry.
    fn share_rows(&mut self, key: &RowsKey, rows: &Arc<FileRows>) {
        let mut shares = [None, None];
        for (slot, half) in Half::BOTH.into_iter().enumerate() {
            let Some((oid, bytes)) = highlight::side_of(rows, half) else {
                continue;
            };
            shares[slot] = self
                .shared
                .with_reader(|reader| reader.share_object(*oid, bytes.clone()));
            if shares[slot].is_none() {
                self.log_missing(Missing::Share);
            }
        }
        // The old registrations drop here, after the new ones were made.
        self.shares.insert(key.clone(), shares);
    }

    /// Plans the windows of both sides, takes the spans the cache has and
    /// submits a task for each of the others.
    fn highlight_rows(&mut self, key: &RowsKey, rows: &Arc<FileRows>) {
        let mut spans = SideSpans::default();
        let Some(path) = self.rows_path(key) else {
            self.highlighted.insert(key.clone(), spans);
            return;
        };
        let Some(lang) = highlight::lang_of(&path) else {
            self.highlighted.insert(key.clone(), spans);
            return;
        };
        for half in Half::BOTH {
            let Some((oid, text)) = highlight::side_of(rows, half) else {
                continue;
            };
            for window in highlight::plan_windows(rows, half, lang.json) {
                let span_key = SpanKey {
                    oid: *oid,
                    query_version: lang.query_version,
                    window: window.clone(),
                };
                if let Some(hit) = self.spans.get(&span_key) {
                    spans.push(half, window, hit);
                    continue;
                }
                let want = (*oid, window.clone());
                if self.plain.contains(&want) {
                    continue;
                }
                if let Some(inflight) = self.inflight.get_mut(&want) {
                    if !inflight.owners.contains(&(key.clone(), half)) {
                        inflight.owners.push((key.clone(), half));
                    }
                    continue;
                }
                let cancel = Arc::new(AtomicBool::new(false));
                let work = Work {
                    key: span_key,
                    text: text.clone(),
                    path: path.clone(),
                    language: lang.name.clone(),
                    cancel: cancel.clone(),
                };
                let tx = self.tx.clone();
                let submitted = self.shared.submit(move || {
                    let job = highlight::run(work);
                    if let Some(tx) = tx {
                        // The receiver is gone with the screen.
                        let _ = tx.unbounded_send(job);
                    }
                });
                if submitted {
                    self.inflight.insert(
                        want,
                        Inflight {
                            cancel,
                            owners: vec![(key.clone(), half)],
                        },
                    );
                } else {
                    self.plain.insert(want);
                }
            }
        }
        self.highlighted.insert(key.clone(), spans);
    }

    /// A diff left the held rows: what was derived from it goes, and a task
    /// nobody waits for any more is cancelled.
    pub(crate) fn drop_rows(&mut self, key: &RowsKey) {
        self.displays.remove(key);
        self.shares.remove(key);
        self.highlighted.remove(key);
        self.widest.remove(key);
        self.hscroll.remove(key);
        self.inflight.retain(|_, inflight| {
            inflight.owners.retain(|(owner, _)| owner != key);
            if inflight.owners.is_empty() {
                inflight.cancel.store(true, Ordering::Relaxed);
                false
            } else {
                true
            }
        });
    }

    /// A highlight task delivered a window's spans.
    pub(crate) fn on_spans(
        &mut self,
        key: SpanKey,
        spans: Arc<WindowSpans>,
        took_us: u64,
        cx: &mut Context<ReviewModel>,
    ) {
        let want = (key.oid, key.window.clone());
        // Nobody waits for it (the rows left, the task was cancelled): drop it.
        let Some(inflight) = self.inflight.remove(&want) else {
            return;
        };
        debug!(
            "highlight";
            id = self.id.0,
            bytes = key.window.end - key.window.start,
            outcome = "done",
            took_us = took_us
        );
        self.spans.insert(key.clone(), spans.clone());
        let mut painted = false;
        for (rows_key, half) in inflight.owners {
            let Some(rows) = self.rows.get(&rows_key) else {
                continue;
            };
            let same = highlight::side_of(rows, half).is_some_and(|(oid, _)| *oid == key.oid);
            if same {
                self.highlighted.entry(rows_key).or_default().push(
                    half,
                    key.window.clone(),
                    spans.clone(),
                );
                painted = true;
            }
        }
        if painted {
            cx.notify();
        }
    }

    /// A highlight task gave no spans: the window stays plain text for this
    /// screen and is not tried again.
    pub(crate) fn on_spans_unavailable(
        &mut self,
        key: SpanKey,
        unavailable: bool,
        language: &str,
        bytes: u64,
        took_us: u64,
        task: &Arc<AtomicBool>,
    ) {
        let want = (key.oid, key.window);
        // Only the answer of the task now waited for counts: a task that was
        // cancelled and replaced may answer after its replacement started.
        if !self
            .inflight
            .get(&want)
            .is_some_and(|inflight| Arc::ptr_eq(&inflight.cancel, task))
        {
            return;
        }
        self.inflight.remove(&want);
        self.plain.insert(want);
        debug!(
            "highlight";
            id = self.id.0,
            bytes = bytes,
            outcome = if unavailable { "unavailable" } else { "timed_out" },
            took_us = took_us
        );
        if unavailable && self.shared.first_warning(language) {
            lectern_logging::warn!("highlight unavailable"; id = self.id.0, bytes = bytes);
        }
    }

    // ----- unified / side by side ---------------------------------------

    /// Switches how diffs are laid out: the display rows and the items are
    /// built again from the rows already held (the mode is not part of the
    /// `RowsKey`), the scroll position and the selection stay on their rows,
    /// and the content area gets the width the mode needs.
    pub(crate) fn set_mode(&mut self, mode: DiffMode, cx: &mut Context<ReviewModel>) {
        if self.mode == mode {
            return;
        }
        self.mode = mode;
        self.shared.set_mode(mode);
        self.shared.dispatch(WorkspaceCommand::SetReadingColumn(
            mode == DiffMode::Unified,
        ));
        self.displays = self
            .rows
            .iter()
            .map(|(key, rows)| {
                let display: Arc<[DisplayRow]> = items::display_rows(rows, mode).into();
                (key.clone(), display)
            })
            .collect();
        self.rebuild_in_place(cx);
    }

    pub(crate) fn toggle_mode(&mut self, cx: &mut Context<ReviewModel>) {
        let next = match self.mode {
            DiffMode::Unified => DiffMode::SideBySide,
            DiffMode::SideBySide => DiffMode::Unified,
        };
        self.set_mode(next, cx);
    }

    // ----- folds ---------------------------------------------------------

    /// Shows a folded run: the file's context grows by the run's length (the
    /// run is then shorter than what the hunks around it show), so the diffs
    /// of that file are asked for again under a new `RowsKey`. Their old
    /// height is kept for the placeholder, so the list does not jump.
    pub(crate) fn expand_fold(&mut self, rows: &RowsKey, ix: u32, cx: &mut Context<ReviewModel>) {
        let Some(file) = items::rows_file(&self.snapshot, rows) else {
            return;
        };
        let Some(fold) = self
            .displays
            .get(rows)
            .and_then(|display| display.get(ix as usize))
            .and_then(|row| row.fold.clone())
        else {
            return;
        };
        let count = fold.end.saturating_sub(fold.start);
        let current = self.context.get(&file).copied().unwrap_or(DEFAULT_CONTEXT);
        let target = current.saturating_add(count.max(1));
        self.context.insert(file, target);
        let grown = match rows {
            RowsKey::Final { file, .. } => RowsKey::Final {
                file: file.clone(),
                context: target,
            },
            RowsKey::Step { step, .. } => RowsKey::Step {
                step: *step,
                context: target,
            },
        };
        let lines = self.known_lines.get(rows).copied().unwrap_or(1);
        self.known_lines
            .insert(grown, lines.saturating_add(count).max(1));
        self.rebuild_in_place(cx);
    }

    /// ↵: expands the selected fold; with a hunk or a row selected, the first
    /// fold after it in the same diff.
    pub(crate) fn expand_selected_fold(&mut self, cx: &mut Context<ReviewModel>) -> bool {
        let Some(selected) = self.selection.clone() else {
            return false;
        };
        let Some(at) = anchor::find(&self.items, &selected) else {
            return false;
        };
        let target = match &self.items[at] {
            Item::Fold { rows, ix, .. } => Some((rows.clone(), *ix)),
            Item::Hunk { rows, .. } | Item::Row { rows, .. } => self.items[at..]
                .iter()
                .take_while(|item| item.rows_key() == Some(rows))
                .find_map(|item| match item {
                    Item::Fold { rows, ix, .. } => Some((rows.clone(), *ix)),
                    _ => None,
                }),
            _ => None,
        };
        let Some((rows, ix)) = target else {
            return false;
        };
        self.expand_fold(&rows, ix, cx);
        true
    }

    /// The key of the item a fold click selects.
    pub(crate) fn fold_key(rows: &RowsKey, old: u32) -> ItemKey {
        ItemKey::Fold {
            rows: rows.clone(),
            old,
        }
    }
}
