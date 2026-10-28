use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use lectern_kernel::{ActivationToken, Event, Service, service};

use super::*;
use crate::changes::{
    ChangeDetail, ChangeError, ChangePath, ChangeStatus, ChangeTotals, DiffHunk, FileChange,
    FileMode, FileSide, SideIdentity, SideText,
};
use crate::project::{BlobId, RelPath};
use crate::session::{Agent, ContentRef, SourceId, Timestamp, ToolCallId, ToolStatus, TurnId};

/// `Probe::<T>::IS_SERVICE` is true exactly when `T: Service`: the inherent
/// constant only applies when its bound holds, otherwise the blanket trait's
/// constant does.
struct Probe<T: ?Sized>(PhantomData<T>);

trait NotService {
    const IS_SERVICE: bool = false;
}

impl<T: ?Sized> NotService for T {}

impl<T: ?Sized + Service> Probe<T> {
    const IS_SERVICE: bool = true;
}

/// A `Review` with nothing open, to check the trait is usable as a shared
/// trait object.
struct Empty;

impl Review for Empty {
    fn open(&self, _target: ReviewTarget) -> ReviewId {
        ReviewId(1)
    }
    fn snapshot(&self, _id: ReviewId) -> Option<Arc<ReviewSnapshot>> {
        None
    }
    fn command(&self, _id: ReviewId, _command: ReviewCommand) {}
    fn rows(&self, _id: ReviewId, _key: &RowsKey) -> Option<Arc<FileRows>> {
        None
    }
    fn text(&self, _id: ReviewId, _key: &TextKey) -> Option<Arc<str>> {
        None
    }
    fn close(&self, _id: ReviewId) {}
    fn overview(&self, _project: &ProjectKey) -> Option<Arc<Overview>> {
        None
    }
    fn watch_overview(&self, _project: &ProjectKey) -> OverviewInterest {
        OverviewInterest::new(|| {})
    }
    fn register_brief_rules(
        &self,
        _provider: Arc<dyn BriefRules>,
        _token: ActivationToken,
    ) -> BriefRulesRegistration {
        BriefRulesRegistration::new(|| {})
    }
}

/// A provider that answers in two batches and stops once cancelled.
struct TwoBatches;

impl BriefRules for TwoBatches {
    fn id(&self) -> Arc<str> {
        text("code-intel-brief")
    }
    fn evaluate(&self, request: BriefRequest, reply: BriefReply) {
        for (i, file) in request.files.iter().enumerate() {
            if reply.cancelled() {
                return;
            }
            let done = i + 1 == request.files.len();
            reply.send(vec![provided(Some(file.key.clone()))], done);
        }
    }
}

fn path(p: &str) -> Arc<Path> {
    Arc::from(PathBuf::from(p))
}

fn text(s: &str) -> Arc<str> {
    Arc::from(s)
}

fn change_path(p: &[u8]) -> ChangePath {
    ChangePath(Arc::from(p))
}

fn shareable<T: Send + Sync + 'static>() {}

fn sendable<T: Send + 'static>() {}

/// Serializes to JSON (the process plugin protocol is JSON-RPC, ADR 0018)
/// and back.
fn round_trip<T>(value: &T) -> T
where
    T: serde::Serialize + serde::de::DeserializeOwned,
{
    let json = serde_json::to_string(value).expect("serializes");
    serde_json::from_str(&json).expect("deserializes")
}

fn project() -> ProjectKey {
    ProjectKey::Git {
        common_dir: path("/r/.git"),
    }
}

fn worktree() -> WorktreeKey {
    WorktreeKey(path("/r/.git"))
}

fn session(id: &str) -> SessionKey {
    SessionKey {
        source: SourceId(text("session-claude-code")),
        id: text(id),
    }
}

fn call(id: &str) -> ToolCallId {
    ToolCallId(text(id))
}

fn oid(byte: u8) -> BlobId {
    BlobId::Sha1([byte; 20])
}

fn modified(p: &str) -> FileKey {
    FileKey {
        old: Some(change_path(p.as_bytes())),
        new: Some(change_path(p.as_bytes())),
    }
}

fn turn(session: u16, ordinal: u32) -> TurnRef {
    TurnRef { session, ordinal }
}

fn text_key(session: u16, pos: u64) -> TextKey {
    TextKey {
        session,
        at: ContentRef::new(pos, 120, 1),
    }
}

fn provided(file: Option<FileKey>) -> ProvidedItem {
    ProvidedItem {
        rule: text("callers-not-updated"),
        critical: true,
        file,
        line: Some(LineAnchor {
            side: DiffSide::New,
            line: 12,
        }),
        args: Arc::from(vec![(text("callers"), text("3"))]),
    }
}

fn hunk() -> DiffHunk {
    DiffHunk {
        old: 3..5,
        new: 3..9,
        old_byte: 40,
        new_byte: 40,
    }
}

fn entry(key: FileKey, group: GroupKind, state: AttributionState) -> FileEntry {
    FileEntry {
        change: FileChange {
            status: ChangeStatus::Modified,
            path: key.new.clone().unwrap(),
            old: Some(FileSide {
                mode: FileMode::File,
                content: SideIdentity::Blob(oid(1)),
            }),
            new: Some(FileSide {
                mode: FileMode::File,
                content: SideIdentity::Blob(oid(2)),
            }),
            detail: ChangeDetail::Text {
                hunks: Arc::from(vec![hunk()]),
                added: 6,
                removed: 2,
            },
            generated: false,
            untracked: false,
            staged: None,
        },
        key,
        noise: None,
        group,
        package: Some(text("crates/app")),
        attribution: FileAttribution {
            state,
            turns: Arc::from(vec![turn(0, 1), turn(1, 1)]),
            commands: Arc::from(vec![CallRef {
                turn: turn(0, 2),
                call: call("toolu_fmt"),
            }]),
            other_sessions: Arc::from(vec![session("other")]),
            drifted_hunks: 1,
        },
    }
}

/// Every snapshot field set, with one of each variant across the files,
/// turns and brief.
fn snapshot() -> ReviewSnapshot {
    let lock = FileKey {
        old: None,
        new: Some(change_path(b"Cargo.lock")),
    };
    let mut noise = entry(
        lock.clone(),
        GroupKind::Noise(NoiseKind::Lockfile),
        AttributionState::FileOnly,
    );
    noise.noise = Some(NoiseKind::Lockfile);
    let files = vec![
        entry(
            modified("src/lib.rs"),
            GroupKind::Code,
            AttributionState::Lines,
        ),
        entry(
            modified("tests/a.rs"),
            GroupKind::Tests,
            AttributionState::Pending,
        ),
        entry(
            modified("ci.yml"),
            GroupKind::Config,
            AttributionState::None,
        ),
        entry(
            FileKey {
                old: Some(change_path(b"old\xff.md")),
                new: Some(change_path(b"docs/new.md")),
            },
            GroupKind::Docs,
            AttributionState::Lines,
        ),
        entry(
            modified("Cargo.toml"),
            GroupKind::Deps,
            AttributionState::Lines,
        ),
        entry(
            modified("gen.rs"),
            GroupKind::Noise(NoiseKind::Generated),
            AttributionState::Lines,
        ),
        entry(
            modified("snap.snap"),
            GroupKind::Noise(NoiseKind::Snapshot),
            AttributionState::Lines,
        ),
        noise,
    ];
    let items = vec![
        NarrativeItem::AgentText(text_key(0, 10)),
        NarrativeItem::Injected(text_key(0, 20)),
        NarrativeItem::Step(StepEntry {
            step: StepId(0),
            file: modified("src/lib.rs"),
            calls: Arc::from(vec![call("toolu_1"), call("toolu_2")]),
            added: 6,
            removed: 2,
            alignment: Alignment::Aligned,
            survival: Survival::All,
        }),
        NarrativeItem::Step(StepEntry {
            step: StepId(1),
            file: modified("tests/a.rs"),
            calls: Arc::from(vec![call("toolu_3")]),
            added: 1,
            removed: 0,
            alignment: Alignment::Partial {
                failed: Arc::from(vec![call("toolu_3")]),
            },
            survival: Survival::Partly,
        }),
        NarrativeItem::Step(StepEntry {
            step: StepId(2),
            file: modified("ci.yml"),
            calls: Arc::from(vec![call("toolu_4")]),
            added: 0,
            removed: 4,
            alignment: Alignment::Unaligned,
            survival: Survival::None,
        }),
        NarrativeItem::Command {
            call: call("toolu_fmt"),
            name: text("Bash"),
            files: Arc::from(vec![lock.clone()]),
        },
        NarrativeItem::Subagent {
            session: 1,
            call: call("toolu_task"),
        },
        NarrativeItem::Tool {
            call: call("toolu_read"),
            name: text("Read"),
            status: Some(ToolStatus::Denied),
        },
        NarrativeItem::Tool {
            call: call("toolu_wait"),
            name: text("Wait"),
            status: None,
        },
    ];
    let rules = vec![
        BriefRule::ChangedSinceSeen,
        BriefRule::Drifted,
        BriefRule::EditFailed,
        BriefRule::CommandOnly,
        BriefRule::Sensitive(SensitiveKind::AgentInstructions),
        BriefRule::Sensitive(SensitiveKind::Ci),
        BriefRule::Sensitive(SensitiveKind::Hooks),
        BriefRule::Sensitive(SensitiveKind::GitMetadata),
        BriefRule::Sensitive(SensitiveKind::Symlink),
        BriefRule::Sensitive(SensitiveKind::Executable),
        BriefRule::Sensitive(SensitiveKind::Submodule),
        BriefRule::Deleted,
        BriefRule::Incomplete(IncompleteKind::Truncated),
        BriefRule::Incomplete(IncompleteKind::TooLarge),
        BriefRule::Incomplete(IncompleteKind::Binary),
        BriefRule::Incomplete(IncompleteKind::NotRetained),
        BriefRule::Incomplete(IncompleteKind::Filtered),
        BriefRule::Incomplete(IncompleteKind::Unavailable),
        BriefRule::Incomplete(IncompleteKind::Listed),
        BriefRule::Noise(NoiseKind::Snapshot),
        BriefRule::Provided {
            provider: text("code-intel-brief"),
            rule: text("public-api"),
            args: Arc::from(vec![(text("name"), text("run"))]),
        },
    ];
    let items_brief: Vec<BriefItem> = rules
        .into_iter()
        .enumerate()
        .map(|(i, rule)| BriefItem {
            rule,
            critical: i % 2 == 0,
            file: (i % 3 != 0).then(|| modified("src/lib.rs")),
            line: (i % 4 == 0).then_some(LineAnchor {
                side: if i % 8 == 0 {
                    DiffSide::Old
                } else {
                    DiffSide::New
                },
                line: i as u32,
            }),
            count: 1 + i as u32,
        })
        .collect();
    ReviewSnapshot {
        id: ReviewId(7),
        target: ReviewTarget::Session(session("main")),
        generation: 12,
        state: LoadState::Ready,
        base: Some(BaseInfo {
            commit: Some(oid(0xba)),
            how: BaseHow::Reflog,
        }),
        sessions: Arc::from(vec![
            SessionRef {
                key: session("main"),
                agent: Agent::ClaudeCode,
                spawned_by: None,
            },
            SessionRef {
                key: session("main/agent-1"),
                agent: Agent::Other(text("third-party")),
                spawned_by: Some((turn(0, 2), call("toolu_task"))),
            },
        ]),
        seen: Arc::from(vec![
            SeenState::Unseen,
            SeenState::Seen,
            SeenState::ChangedSinceSeen {
                since: Timestamp(1_760_000_000_000),
            },
            SeenState::Unseen,
            SeenState::Unseen,
            SeenState::Unseen,
            SeenState::Unseen,
            SeenState::Unseen,
        ]),
        files: Arc::from(files),
        unclaimed: 2,
        progress: Progress {
            total: 8,
            seen: 1,
            changed_since_seen: 1,
        },
        turns: Some(Arc::from(vec![
            TurnEntry {
                at: turn(0, 1),
                id: TurnId(text("t-1")),
                started_at: Timestamp(1),
                ended_at: Some(Timestamp(2)),
                prompt: Some(text_key(0, 0)),
                prompt_preview: text("Fix the parser"),
                items: Arc::from(items),
                after_last_seen: false,
            },
            TurnEntry {
                at: turn(1, 1),
                id: TurnId(text("t-sub")),
                started_at: Timestamp(3),
                ended_at: None,
                prompt: None,
                prompt_preview: text(""),
                items: Arc::from(Vec::new()),
                after_last_seen: true,
            },
        ])),
        groups: Arc::from(vec![
            Group {
                kind: GroupKind::Code,
                files: Arc::from(vec![0]),
            },
            Group {
                kind: GroupKind::Noise(NoiseKind::Lockfile),
                files: Arc::from(vec![7]),
            },
        ]),
        brief: Arc::new(Brief {
            items: Arc::from(items_brief),
            omitted: 9,
            pending_providers: 1,
            analysis: AnalysisState::Unavailable(text("not implemented")),
        }),
        focus_review: Arc::from(vec![text("src/lib.rs")]),
        focus_touched: Arc::from(vec![text("src/lib.rs"), text("README.md")]),
        totals: ChangeTotals {
            files: 10,
            added: 40,
            removed: 9,
            ..ChangeTotals::default()
        },
        degraded: None,
        notes: Arc::from(Vec::<Note>::new()),
    }
}

#[test]
fn review_service_id_and_multiple() {
    assert_eq!(<dyn Review as Service>::ID, "review");
    const { assert!(!<dyn Review as Service>::MULTIPLE) };
    let key = service::<dyn Review>();
    assert_eq!(key.id(), "review");
    assert!(!key.multiple());
}

#[test]
fn brief_rules_is_not_a_service() {
    // Providers register through `Review::register_brief_rules`; a
    // `MULTIPLE` service would make `review` wait for one to activate.
    const { assert!(!Probe::<dyn BriefRules>::IS_SERVICE) };
    // The probe does tell services apart.
    const { assert!(Probe::<dyn Review>::IS_SERVICE) };
}

#[test]
fn review_is_a_shared_trait_object() {
    let review: Arc<dyn Review> = Arc::new(Empty);
    let id = review.open(ReviewTarget::Outside {
        project: project(),
        worktree: worktree(),
    });
    assert!(review.snapshot(id).is_none());
    review.command(id, ReviewCommand::LooksGood);
    let key = RowsKey::Final {
        file: modified("src/lib.rs"),
        context: 3,
    };
    assert!(review.rows(id, &key).is_none());
    assert!(review.text(id, &text_key(0, 0)).is_none());
    assert!(review.overview(&project()).is_none());
    drop(review.watch_overview(&project()));
    review.close(id);
    // `register_brief_rules` needs a token from a real activation; its
    // signature is checked by `Empty`, its registration below.

    // Snapshots and rows cross threads inside `Arc`s; registrations and
    // interests go to `PluginHandle::own` or a view's state (`Send`),
    // replies to any thread.
    shareable::<Arc<dyn Review>>();
    shareable::<Arc<dyn BriefRules>>();
    shareable::<ReviewSnapshot>();
    shareable::<FileRows>();
    shareable::<Overview>();
    shareable::<ReviewEvent>();
    sendable::<BriefRulesRegistration>();
    sendable::<OverviewInterest>();
    shareable::<BriefReply>();
}

#[test]
fn brief_rules_registration_releases_once_on_drop() {
    let count = Arc::new(AtomicUsize::new(0));
    let c = count.clone();
    let registration = BriefRulesRegistration::new(move || {
        c.fetch_add(1, Ordering::SeqCst);
    });
    assert_eq!(count.load(Ordering::SeqCst), 0);
    assert_eq!(format!("{registration:?}"), "BriefRulesRegistration { .. }");
    // Dropped on another thread, as a plugin's owned values may be.
    std::thread::spawn(move || drop(registration))
        .join()
        .unwrap();
    assert_eq!(count.load(Ordering::SeqCst), 1);
}

#[test]
fn overview_interest_releases_once_on_drop() {
    let count = Arc::new(AtomicUsize::new(0));
    let c = count.clone();
    let interest = OverviewInterest::new(move || {
        c.fetch_add(1, Ordering::SeqCst);
    });
    assert_eq!(count.load(Ordering::SeqCst), 0);
    assert_eq!(format!("{interest:?}"), "OverviewInterest { .. }");
    std::thread::spawn(move || drop(interest)).join().unwrap();
    assert_eq!(count.load(Ordering::SeqCst), 1);
}

#[test]
fn brief_reply_delivers_batches_until_cancelled() {
    let received = Arc::new(Mutex::new(Vec::new()));
    let r = received.clone();
    let cancelled = Arc::new(AtomicBool::new(false));
    let reply = BriefReply::new(
        move |items, done| r.lock().unwrap().push((items, done)),
        cancelled.clone(),
    );
    assert!(!reply.cancelled());
    assert_eq!(format!("{reply:?}"), "BriefReply { cancelled: false, .. }");

    let files: Vec<BriefFile> = ["a.rs", "b.rs"]
        .iter()
        .map(|p| BriefFile {
            key: modified(p),
            path: text(p),
            old_oid: Some(oid(1)),
            new_oid: None,
            hunks: Arc::from(vec![hunk()]),
        })
        .collect();
    let request = BriefRequest {
        project: project(),
        worktree: Some(worktree()),
        files: Arc::from(files),
    };
    let provider: Arc<dyn BriefRules> = Arc::new(TwoBatches);
    assert_eq!(&*provider.id(), "code-intel-brief");
    // The provider may answer from any thread.
    let p = provider.clone();
    let req = request.clone();
    std::thread::spawn(move || p.evaluate(req, reply))
        .join()
        .unwrap();
    {
        let got = received.lock().unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0], (vec![provided(Some(modified("a.rs")))], false));
        assert_eq!(got[1], (vec![provided(Some(modified("b.rs")))], true));
    }

    // Once `review` drops the request, the provider sees it and stops.
    cancelled.store(true, Ordering::Release);
    let r = received.clone();
    let reply = BriefReply::new(
        move |items, done| r.lock().unwrap().push((items, done)),
        cancelled,
    );
    assert!(reply.cancelled());
    provider.evaluate(request, reply);
    assert_eq!(received.lock().unwrap().len(), 2);
}

#[test]
fn review_event_round_trips() {
    fn event<E: Event + Clone>() {}
    event::<ReviewEvent>();

    let events = [
        ReviewEvent::Changed {
            id: ReviewId(1),
            generation: 2,
        },
        ReviewEvent::Prepared {
            id: ReviewId(u64::MAX),
            generation: 0,
        },
        ReviewEvent::Overview {
            project: ProjectKey::Folder { root: path("/f") },
            generation: 5,
        },
    ];
    for e in events {
        assert_eq!(round_trip(&e), e);
    }
}

#[test]
fn targets_and_commands_round_trip() {
    let targets = [
        ReviewTarget::Session(session("s-1")),
        ReviewTarget::Outside {
            project: project(),
            worktree: worktree(),
        },
        ReviewTarget::Range {
            project: project(),
            worktree: WorktreeKey(path("/r/.git/worktrees/w")),
            base: crate::changes::BaseSpec::MergeBase(text("refs/heads/main")),
        },
    ];
    for t in targets {
        assert_eq!(round_trip(&t), t);
    }

    let added = FileKey {
        old: None,
        new: Some(change_path(b"new.rs")),
    };
    let deleted = FileKey {
        old: Some(change_path(b"\xffgone")),
        new: None,
    };
    let commands = [
        ReviewCommand::MarkSeen(added.clone()),
        ReviewCommand::MarkUnseen(deleted),
        ReviewCommand::LooksGood,
        ReviewCommand::Prepare(Arc::from(vec![
            Need::Rows(RowsKey::Final {
                file: added,
                context: 3,
            }),
            Need::Rows(RowsKey::Step {
                step: StepId(4),
                context: u32::MAX,
            }),
            Need::Text(text_key(1, 99)),
        ])),
        ReviewCommand::Refresh,
        ReviewCommand::Analyze,
        ReviewCommand::CancelAnalyze,
    ];
    for c in commands {
        assert_eq!(round_trip(&c), c);
    }
}

#[test]
fn snapshot_round_trips() {
    let s = snapshot();
    assert_eq!(round_trip(&s), s);

    // The other states, bases, degradations and analysis states.
    let states = [
        LoadState::Loading,
        LoadState::Failed(ReviewError::Missing),
        LoadState::Failed(ReviewError::NotInOpenProject),
        LoadState::Failed(ReviewError::Changes(ChangeError::NoMergeBase)),
    ];
    let hows = [
        BaseHow::Recorded,
        BaseHow::BeforeLog,
        BaseHow::CurrentHead,
        BaseHow::Spec,
    ];
    let degraded = [Degraded::UnrecognizedSession, Degraded::FolderProject];
    let analyses = [
        AnalysisState::Idle,
        AnalysisState::Running {
            started_at: Timestamp(-1),
        },
        AnalysisState::Done {
            stale: true,
            partial: false,
        },
        AnalysisState::Failed(text("schema")),
    ];
    for (i, state) in states.into_iter().enumerate() {
        let mut brief = (*s.brief).clone();
        brief.analysis = analyses[i].clone();
        let other = ReviewSnapshot {
            state,
            base: Some(BaseInfo {
                commit: None,
                how: hows[i],
            }),
            turns: None,
            degraded: degraded.get(i).copied(),
            brief: Arc::new(brief),
            notes: Arc::from(vec![Note::default()]),
            ..s.clone()
        };
        assert_eq!(round_trip(&other), other);
    }
    let loading = ReviewSnapshot {
        state: LoadState::Loading,
        base: None,
        ..s
    };
    assert_eq!(round_trip(&loading), loading);
}

#[test]
fn rows_round_trip() {
    let attribution = HunkAttribution {
        lines: true,
        turns: Arc::from(vec![turn(0, 3)]),
        others: Arc::from(vec![session("other")]),
        drifted: 2,
    };
    let rows = FileRows {
        key: RowsKey::Step {
            step: StepId(2),
            context: 3,
        },
        old: RowsSide {
            oid: Some(oid(5)),
            text: SideText::Text(Arc::from(&b"a\nb\n"[..])),
        },
        new: RowsSide {
            oid: None,
            text: SideText::TooLarge { size: 1 << 30 },
        },
        rows: Arc::from(vec![
            DiffRow::Folded {
                old: 0..10,
                new: 0..10,
                old_byte: Some(0),
                new_byte: None,
            },
            DiffRow::Context {
                old: 10,
                new: 10,
                old_byte: Some(100),
                new_byte: Some(100),
                hunk: None,
            },
            DiffRow::Removed {
                old: 11,
                pair: Some(3),
                old_byte: None,
                hunk: 0,
            },
            DiffRow::Added {
                new: 11,
                pair: Some(2),
                new_byte: Some(u64::MAX),
                hunk: 0,
            },
        ]),
        hunks: Arc::from(vec![
            HunkInfo {
                old: 11..12,
                new: 11..12,
                attribution: attribution.clone(),
            },
            HunkInfo {
                old: 20..20,
                new: 20..25,
                attribution: HunkAttribution {
                    lines: false,
                    turns: Arc::from(Vec::new()),
                    others: Arc::from(Vec::new()),
                    drifted: 0,
                },
            },
        ]),
        inline: Arc::from(vec![(
            2,
            InlineSpans {
                old: Arc::from(vec![0..3, 7..9]),
                new: Arc::from(vec![0..5, 6..8]),
            },
        )]),
    };
    assert_eq!(round_trip(&rows), rows);
}

#[test]
fn overview_and_brief_requests_round_trip() {
    let overviews = [
        OutsideCount::Pending,
        OutsideCount::Files(3),
        OutsideCount::Capped(4001),
    ]
    .map(|outside| Overview {
        packages: match outside {
            OutsideCount::Pending => None,
            OutsideCount::Files(_) => Some(Arc::from(vec![
                (RelPath::from("crates/a"), 3),
                (RelPath::from(""), 1),
            ])),
            OutsideCount::Capped(_) => None,
        },
        project: project(),
        generation: 4,
        sessions: Arc::from(vec![
            SessionOverview {
                session: session("a"),
                progress: Some(Progress {
                    total: 5,
                    seen: 2,
                    changed_since_seen: 1,
                }),
                drifted: Some(true),
                critical: 2,
            },
            SessionOverview {
                session: session("b"),
                progress: None,
                drifted: None,
                critical: 0,
            },
        ]),
        outside,
    });
    for o in overviews {
        assert_eq!(round_trip(&o), o);
    }

    let request = BriefRequest {
        project: ProjectKey::Folder { root: path("/f") },
        worktree: None,
        files: Arc::from(vec![BriefFile {
            key: modified("src/lib.rs"),
            path: text("src/lib.rs"),
            old_oid: None,
            new_oid: Some(BlobId::Sha256([3; 32])),
            hunks: Arc::from(vec![hunk()]),
        }]),
    };
    assert_eq!(round_trip(&request), request);
    for item in [provided(None), provided(Some(modified("x")))] {
        assert_eq!(round_trip(&item), item);
    }
}

#[test]
fn needs_review_is_unknown_unseen_or_changed_since_seen() {
    let with = |progress| SessionOverview {
        session: session("a"),
        progress,
        drifted: None,
        critical: 0,
    };
    let p = |total, seen, changed_since_seen| {
        Some(Progress {
            total,
            seen,
            changed_since_seen,
        })
    };
    assert!(with(None).needs_review());
    assert!(with(p(3, 2, 0)).needs_review());
    assert!(with(p(3, 3, 1)).needs_review());
    assert!(!with(p(3, 3, 0)).needs_review());
    assert!(!with(p(0, 0, 0)).needs_review());
}
