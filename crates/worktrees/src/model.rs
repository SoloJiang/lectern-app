//! `WorktreesModel`: the state the screen and the inspector's page share
//! (docs/design/worktrees.md「数据结构」). One per screen, built when the screen
//! is pushed and dropped when it is popped. It holds the snapshot it was last
//! woken with, the selection, the open overlap items, the milestone's
//! bookkeeping and the two timers (the expiry of an Active worktree, and the
//! loading phases), and it sends the commands. It does no IO and calls no
//! blocking service method.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::StreamExt;
use gpui::{App, Context, Entity, Global, Task, WeakEntity};
use lectern_logging::debug;
use lectern_services::project::WorktreeKey;
use lectern_services::review::ChangeCount;
use lectern_services::session::{SessionKey, Timestamp};
use lectern_services::workspace::WorkspaceCommand;
use lectern_ui::motion::{LOADING_PROGRESS_AT, LOADING_SKELETON_AT};
use lectern_ui::{LoadingPhase, loading_phase};

use crate::hub::Hub;
use crate::present;
use crate::snapshot::{RowUsage, WorktreeRow, WorktreesSnapshot};

/// The model of the screen on top, for the toolbar and the inspector's page:
/// both read it from here (as review_view's `Current`).
pub(crate) struct Current(pub WeakEntity<WorktreesModel>);

impl Global for Current {}

/// Makes `model` the one the toolbar and the page read.
pub(crate) fn set_current(model: &Entity<WorktreesModel>, cx: &mut App) {
    cx.set_global(Current(model.downgrade()));
}

/// The model of the screen, if there is one.
pub(crate) fn current(cx: &App) -> Option<Entity<WorktreesModel>> {
    cx.try_global::<Current>()?.0.upgrade()
}

/// The keyboard's moves through the rows (「Action 与快捷键」).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Move {
    Next,
    Previous,
    First,
    Last,
}

/// Some count has not arrived; `since` is when the model saw that first (the
/// executor's clock, so that tests move it).
struct Waiting {
    since: Instant,
    /// Notifies at the two thresholds of `loading_phase`; dropped with this.
    _timers: Task<()>,
}

/// When a worktree's blink happened last: a new round is a new blink.
#[derive(Clone, Copy)]
struct Blink {
    running: bool,
    round: u64,
}

pub struct WorktreesModel {
    hub: Arc<Hub>,
    pub(crate) snapshot: Option<Arc<WorktreesSnapshot>>,
    pub(crate) selected: Option<WorktreeKey>,
    /// The overlap items of the inspector that are open (by the other
    /// worktree); cleared when the selection moves to another worktree.
    pub(crate) expanded: HashSet<WorktreeKey>,
    /// Scroll the selected row into view on the next frame.
    pub(crate) reveal: bool,
    /// The switches the milestone has been reported for (or that were there
    /// when the model was built).
    reported_switches: u64,
    /// When the armed expiry timer fires, and the timer.
    timer: Option<(Timestamp, Task<()>)>,
    waiting: Option<Waiting>,
    blinks: HashMap<WorktreeKey, Blink>,
    _wake: Task<()>,
}

impl WorktreesModel {
    /// Main thread; reads the current snapshot from `hub` (a screen pushed
    /// again later shows the latest at once). Does no IO.
    pub(crate) fn new(hub: Arc<Hub>, cx: &mut Context<Self>) -> WorktreesModel {
        let mut wake = hub.attach_view("screen");
        let task = cx.spawn(async move |this, cx| {
            while let Some(mut latest) = wake.next().await {
                // Several wake-ups are one refresh: the newest snapshot.
                while let Ok(next) = wake.try_recv() {
                    latest = next;
                }
                if this.update(cx, |this, cx| this.apply(latest, cx)).is_err() {
                    break;
                }
            }
        });
        let mut model = WorktreesModel {
            reported_switches: hub.switches_when_shown(),
            snapshot: None,
            hub,
            selected: None,
            expanded: HashSet::new(),
            reveal: false,
            timer: None,
            waiting: None,
            blinks: HashMap::new(),
            _wake: task,
        };
        if let Some(snapshot) = model.hub.snapshot() {
            model.apply(snapshot, cx);
        }
        model
    }

    // ------------------------------------------------------------- reading

    /// The rows in the order the keyboard walks them: Active, then Idle.
    pub(crate) fn rows(&self) -> impl Iterator<Item = &WorktreeRow> {
        self.snapshot
            .iter()
            .flat_map(|snapshot| snapshot.active_rows.iter().chain(snapshot.idle_rows.iter()))
    }

    /// The selected row.
    pub(crate) fn selected_row(&self) -> Option<&WorktreeRow> {
        let selected = self.selected.as_ref()?;
        self.rows().find(|row| row.key == *selected)
    }

    /// What the placeholders show now.
    pub(crate) fn phase(&self, cx: &App) -> LoadingPhase {
        match &self.waiting {
            Some(waiting) => loading_phase(
                cx.background_executor()
                    .now()
                    .saturating_duration_since(waiting.since),
            ),
            None => LoadingPhase::Nothing,
        }
    }

    /// The blink round of the row's dot: 0 until the worktree starts running
    /// after the model saw it idle.
    pub(crate) fn blink_round(&self, key: &WorktreeKey) -> u64 {
        self.blinks.get(key).map_or(0, |blink| blink.round)
    }

    // ---------------------------------------------------------- the snapshot

    /// A snapshot arrived (or the project closed: a snapshot without rows).
    pub(crate) fn apply(&mut self, snapshot: Arc<WorktreesSnapshot>, cx: &mut Context<Self>) {
        if self
            .snapshot
            .as_ref()
            .is_some_and(|shown| Arc::ptr_eq(shown, &snapshot))
        {
            return;
        }
        let before: Vec<WorktreeKey> = self.rows().map(|row| row.key.clone()).collect();
        self.snapshot = Some(snapshot);
        self.keep_selection(&before);
        self.track_blinks();
        self.rearm_timer(cx);
        self.sync_waiting(cx);
        cx.notify();
    }

    /// The selection rules (「数据结构」): the first snapshot selects the
    /// current worktree (the first row when none is); a later one keeps the
    /// worktree by key; when it is gone the row at the same position is
    /// selected (the last one when the position is past the end).
    fn keep_selection(&mut self, before: &[WorktreeKey]) {
        let after: Vec<WorktreeKey> = self.rows().map(|row| row.key.clone()).collect();
        let selected = match self.selected.take() {
            Some(selected) if after.contains(&selected) => Some(selected),
            Some(selected) => {
                let at = before.iter().position(|key| *key == selected).unwrap_or(0);
                after.get(at.min(after.len().saturating_sub(1))).cloned()
            }
            None => self
                .snapshot
                .as_ref()
                .and_then(|snapshot| snapshot.active.clone())
                .filter(|active| after.contains(active))
                .or_else(|| after.first().cloned()),
        };
        self.set_selected(selected);
    }

    fn set_selected(&mut self, selected: Option<WorktreeKey>) {
        if self.selected != selected {
            self.expanded.clear();
        }
        self.selected = selected;
    }

    /// A worktree whose `running` went from 0 to above 0 since the last
    /// snapshot blinks once; the first snapshot blinks nothing.
    fn track_blinks(&mut self) {
        let Some(snapshot) = &self.snapshot else {
            return;
        };
        let first = self.blinks.is_empty();
        let mut next = HashMap::new();
        for row in snapshot.active_rows.iter().chain(snapshot.idle_rows.iter()) {
            let running = row.running > 0;
            let before = self.blinks.get(&row.key).copied();
            let round = match before {
                Some(blink) if running && !blink.running => blink.round + 1,
                Some(blink) => blink.round,
                None if running && !first => 1,
                None => 0,
            };
            next.insert(row.key.clone(), Blink { running, round });
        }
        self.blinks = next;
    }

    /// One one-shot timer at the moment an Active worktree runs out
    /// (`next_expiry`); re-armed only when that moment changes. When it fires
    /// the plugin composes again.
    fn rearm_timer(&mut self, cx: &mut Context<Self>) {
        let wanted = self
            .snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.next_expiry);
        let Some(at) = wanted else {
            self.timer = None;
            return;
        };
        if self.timer.as_ref().is_some_and(|(armed, _)| *armed == at) {
            return;
        }
        let wait_ms = at.0.saturating_sub(self.hub.now().0).max(0).unsigned_abs();
        let wait = Duration::from_secs_f64(wait_ms as f64 / 1000.0);
        debug!("worktrees timer armed"; in_ms = wait_ms);
        let task = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(wait).await;
            let _ = this.update(cx, |this, _| this.hub.request_build());
        });
        self.timer = Some((at, task));
    }

    /// Starts and stops the wait for the counts: the two thresholds of
    /// `loading_phase` each notify once.
    fn sync_waiting(&mut self, cx: &mut Context<Self>) {
        let waiting = self.snapshot.as_deref().is_some_and(counts_pending);
        match (waiting, self.waiting.is_some()) {
            (true, false) => {
                let since = cx.background_executor().now();
                let timers = cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(LOADING_SKELETON_AT).await;
                    if this.update(cx, |_, cx| cx.notify()).is_err() {
                        return;
                    }
                    cx.background_executor()
                        .timer(LOADING_PROGRESS_AT.saturating_sub(LOADING_SKELETON_AT))
                        .await;
                    let _ = this.update(cx, |_, cx| cx.notify());
                });
                self.waiting = Some(Waiting {
                    since,
                    _timers: timers,
                });
            }
            (false, true) => self.waiting = None,
            _ => {}
        }
    }

    /// The screen drew a frame with the current snapshot: the first one whose
    /// files are settled after a switch is the milestone (「性能」). Reported
    /// once per switch.
    pub(crate) fn frame_drawn(&mut self) {
        let Some(snapshot) = &self.snapshot else {
            return;
        };
        if snapshot.files_settled && snapshot.switches > self.reported_switches {
            self.reported_switches = snapshot.switches;
            self.hub.report_milestone(snapshot.switches);
        }
    }

    // ------------------------------------------------------------ commands

    /// ↓ ↑ ⌥↑ ⌥↓: moves the selection through Active and Idle; stops at the
    /// ends.
    pub(crate) fn select_move(&mut self, movement: Move, cx: &mut Context<Self>) {
        let keys: Vec<WorktreeKey> = self.rows().map(|row| row.key.clone()).collect();
        if keys.is_empty() {
            return;
        }
        let at = self
            .selected
            .as_ref()
            .and_then(|selected| keys.iter().position(|key| key == selected));
        let last = keys.len() - 1;
        let index = match (movement, at) {
            (Move::First, _) | (Move::Next, None) => 0,
            (Move::Last, _) | (Move::Previous, None) => last,
            (Move::Next, Some(i)) => (i + 1).min(last),
            (Move::Previous, Some(i)) => i.saturating_sub(1),
        };
        self.set_selected(Some(keys[index].clone()));
        self.reveal = true;
        cx.notify();
    }

    /// A click on a row: selects it and reviews its changes.
    pub(crate) fn open_worktree(&mut self, key: WorktreeKey, cx: &mut Context<Self>) {
        if !self.rows().any(|row| row.key == key) {
            return;
        }
        self.set_selected(Some(key));
        self.review_changes(cx);
        cx.notify();
    }

    /// ↵ and the inspector's button: the selected worktree's changes against
    /// the base. A prunable worktree has none to review.
    pub(crate) fn review_changes(&mut self, cx: &mut Context<Self>) {
        let (Some(snapshot), Some(row)) = (&self.snapshot, self.selected_row()) else {
            return;
        };
        if row.prunable {
            return;
        }
        let (command, target) = present::review_range(snapshot, &row.key);
        self.hub.dispatch(command, target);
        cx.notify();
    }

    /// A session of the inspector: its review.
    pub(crate) fn open_session(&mut self, key: SessionKey, cx: &mut Context<Self>) {
        self.hub.dispatch(present::review_session(key), "session");
        cx.notify();
    }

    /// An overlap item of the inspector opens or closes.
    pub(crate) fn toggle_overlap(&mut self, key: WorktreeKey, cx: &mut Context<Self>) {
        if !self.expanded.remove(&key) {
            self.expanded.insert(key);
        }
        cx.notify();
    }

    /// The inspector's button: the selected worktree becomes the current
    /// one. The current worktree and a prunable one cannot be switched to.
    /// The screen stays (the current mark moves to the new row).
    pub(crate) fn switch_to(&mut self, cx: &mut Context<Self>) {
        let (Some(snapshot), Some(row)) = (&self.snapshot, self.selected_row()) else {
            return;
        };
        if row.is_active || row.prunable {
            return;
        }
        let worktree = row.key.clone();
        let project = snapshot.project.clone();
        self.hub.log_switch(&project, &worktree);
        self.hub
            .dispatch(WorkspaceCommand::SetActiveWorktree(worktree), "switch");
        cx.notify();
    }

    /// ⌘R and the toolbar's Refresh.
    pub(crate) fn refresh(&mut self) {
        if let Some(snapshot) = &self.snapshot {
            self.hub.refresh(&snapshot.project);
        }
    }
}

/// Whether some count a row waits for has not arrived: the changes, the
/// overlaps or the usage.
pub(crate) fn counts_pending(snapshot: &WorktreesSnapshot) -> bool {
    snapshot
        .active_rows
        .iter()
        .chain(snapshot.idle_rows.iter())
        .any(|row| {
            row.changes.uncommitted == ChangeCount::Pending
                || row.changes.vs_base == ChangeCount::Pending
                || matches!(row.overlaps, crate::snapshot::Overlaps::Pending)
                || matches!(row.usage, RowUsage::Pending)
        })
}
