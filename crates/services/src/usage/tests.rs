use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use lectern_kernel::{ActivationToken, Event, Service, service};

use super::*;
use crate::project::BlobId;
use crate::session::{Agent, RawTokens, SourceId, Timestamp, TurnId};

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

/// A `Usage` with nothing computed, to check the trait is usable as a shared
/// trait object.
struct Empty;

impl Usage for Empty {
    fn watch(&self, _query: UsageQuery) -> UsageInterest {
        UsageInterest::new(|| {})
    }
    fn snapshot(&self, _query: &UsageQuery) -> Option<Arc<UsageSnapshot>> {
        None
    }
    fn limits(&self) -> Arc<Limits> {
        Arc::new(Limits::default())
    }
    fn command(&self, _command: UsageCommand) {}
    fn begin_analysis(&self, _start: AnalysisStart) -> Result<AnalysisId, UsageError> {
        Err(UsageError::Unavailable)
    }
    fn meter(&self, _event: MeterEvent) -> Result<(), UsageError> {
        Err(UsageError::UnknownAnalysis)
    }
    fn register_mr_source(
        &self,
        _provider: Arc<dyn MrSource>,
        _token: ActivationToken,
    ) -> MrSourceRegistration {
        MrSourceRegistration::new(|| {})
    }
}

/// An MR source that knows one repository and one MR.
struct OneMr;

impl MrSource for OneMr {
    fn repo(&self, remote_url: &str) -> Result<Option<ForgeRepo>, MrSourceError> {
        match remote_url {
            "https://forge.test/team/app.git" => Ok(Some(forge_repo())),
            "https://offline.test/x.git" => Err(MrSourceError::Offline),
            _ => Ok(None),
        }
    }
    fn mrs_for_branch(
        &self,
        repo: &ForgeRepo,
        branch: &str,
    ) -> Result<Arc<[MrInfo]>, MrSourceError> {
        if *repo == forge_repo() && branch == "feat" {
            Ok(Arc::from(vec![mr_info()]))
        } else {
            Ok(Arc::from(Vec::new()))
        }
    }
    fn mrs_containing(
        &self,
        _repo: &ForgeRepo,
        commit: &BlobId,
    ) -> Result<Arc<[MrInfo]>, MrSourceError> {
        if *commit == oid(3) {
            Ok(Arc::from(vec![mr_info()]))
        } else {
            Err(MrSourceError::NotFound)
        }
    }
}

fn path(p: &str) -> Arc<Path> {
    Arc::from(PathBuf::from(p))
}

fn text(s: &str) -> Arc<str> {
    Arc::from(s)
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
    WorktreeKey(path("/r/.git/worktrees/feat"))
}

fn session(id: &str) -> SessionKey {
    SessionKey {
        source: SourceId(text("session-codex")),
        id: text(id),
    }
}

fn turn(id: &str) -> TurnId {
    TurnId(text(id))
}

fn oid(byte: u8) -> BlobId {
    BlobId::Sha1([byte; 20])
}

fn id(byte: u8) -> UsageProjectId {
    UsageProjectId([byte; 16])
}

fn date(day: u8) -> LocalDate {
    LocalDate {
        year: 2026,
        month: 10,
        day,
    }
}

fn tokens(n: u64) -> Tokens {
    Tokens {
        input: n,
        output: n + 1,
        cache_read: n + 2,
        cache_write: n + 3,
    }
}

fn forge_repo() -> ForgeRepo {
    ForgeRepo {
        host: text("forge.test"),
        id: text("12345"),
    }
}

fn mr_ref(number: u64) -> MrRef {
    MrRef {
        repo: forge_repo(),
        number,
    }
}

fn mr_label(number: u64, state: MrState) -> MrLabel {
    MrLabel {
        mr: mr_ref(number),
        title: text("Add usage"),
        state,
    }
}

fn mr_info() -> MrInfo {
    let fork = ForgeRepo {
        host: text("forge.test"),
        id: text("67890"),
    };
    MrInfo {
        mr: mr_ref(7),
        title: text("Add usage"),
        source: (fork, text("feat")),
        target: (forge_repo(), text("main")),
        head: oid(3),
        bases: Arc::from(vec![oid(1), BlobId::Sha256([2; 32])]),
        merged_target: Some(oid(4)),
        timeline: Arc::from(vec![
            (Timestamp(1_000), MrEventKind::Opened),
            (Timestamp(2_000), MrEventKind::Closed),
            (Timestamp(3_000), MrEventKind::Reopened),
            (Timestamp(4_000), MrEventKind::Merged),
        ]),
    }
}

fn branch() -> BranchLabel {
    BranchLabel {
        name: text("refs/heads/feat"),
        worktree: Some(worktree()),
    }
}

fn header(state: UsageState) -> SnapshotHeader {
    SnapshotHeader {
        generation: 3,
        state,
        may_include_inherited: true,
    }
}

fn info(status: ProjectStatus) -> ProjectInfo {
    ProjectInfo {
        id: id(1),
        name: text("app"),
        folder: false,
        key: Some(project()),
        status,
    }
}

fn anthropic() -> RawTokens {
    RawTokens::Anthropic {
        input: 10,
        output: 20,
        cache_read: 30,
        cache_write: 40,
        cache_write_5m: 25,
        cache_write_1h: 15,
    }
}

fn openai() -> RawTokens {
    RawTokens::OpenAi {
        input: 100,
        cached_input: 60,
        cache_write: 0,
        output: 50,
        reasoning_output: u64::MAX,
    }
}

fn rows() -> Arc<[RowKey]> {
    Arc::from(vec![
        RowKey::Mrs {
            mrs: Arc::from(vec![
                mr_label(7, MrState::Open),
                mr_label(8, MrState::Merged),
                mr_label(9, MrState::Closed),
            ]),
            branch: branch(),
        },
        RowKey::Branch {
            use_id: UseId([0xab; 16]),
            branch: BranchLabel {
                name: text("refs/heads/fix"),
                worktree: None,
            },
        },
        RowKey::Unassigned {
            unattributable: tokens(5),
        },
    ])
}

/// One snapshot of each variant, with every state, status and row key
/// spread across them.
fn snapshots() -> Vec<UsageSnapshot> {
    let daily: Arc<[DayTotals]> = Arc::from(vec![
        Arc::from(vec![(Agent::ClaudeCode, 10), (Agent::Codex, 0)]),
        Arc::from(Vec::new()),
        Arc::from(vec![(Agent::Other(text("pi")), u64::MAX)]),
    ]);
    let days: Arc<[LocalDate]> = Arc::from(vec![date(20), date(21), date(22)]);
    let project_usage = ProjectUsage {
        header: header(UsageState::Ready),
        project: info(ProjectStatus::Active),
        window: Window::D7,
        days: days.clone(),
        total: tokens(100),
        rows: rows()
            .iter()
            .map(|key| UsageRow {
                key: key.clone(),
                tokens: tokens(30),
                review: tokens(1),
            })
            .collect(),
        by_model: Arc::from(vec![
            (Agent::ClaudeCode, text("model-a"), tokens(60)),
            (Agent::Codex, text("model-b"), tokens(40)),
        ]),
        by_agent: Arc::from(vec![
            (Agent::ClaudeCode, tokens(60)),
            (Agent::Codex, tokens(40)),
        ]),
        daily: daily.clone(),
    };
    let all = AllUsage {
        header: header(UsageState::Loading),
        window: Window::D30,
        days,
        total: tokens(200),
        projects: Arc::from(vec![
            (info(ProjectStatus::PathMissing), tokens(50)),
            (
                ProjectInfo {
                    id: id(2),
                    name: text("notes"),
                    folder: true,
                    key: Some(ProjectKey::Folder {
                        root: path("/home/u/notes"),
                    }),
                    status: ProjectStatus::Temporary {
                        reason: TemporaryReason::LocalRewrite,
                        candidate: Some(id(1)),
                    },
                },
                tokens(20),
            ),
            (
                ProjectInfo {
                    id: id(3),
                    name: text("old"),
                    folder: false,
                    key: None,
                    status: ProjectStatus::Merged {
                        into: id(1),
                        alias: AliasId(9),
                    },
                },
                tokens(0),
            ),
        ]),
        other: tokens(7),
        daily,
    };
    let worktree_usage = WorktreeUsage {
        header: header(UsageState::Backfilling {
            done: 4096,
            total: u64::MAX,
        }),
        branch: Some(branch()),
        mrs: Arc::from(vec![mr_label(7, MrState::Open)]),
        cumulative: Some(tokens(12)),
        review: tokens(2),
        since: Some(Timestamp(-5)),
    };
    let detached = WorktreeUsage {
        header: header(UsageState::Degraded(UsageDegraded::NewerDatabase)),
        branch: None,
        mrs: Arc::from(Vec::new()),
        cumulative: None,
        review: Tokens::default(),
        since: None,
    };
    let session_usage = SessionUsage {
        header: header(UsageState::Degraded(UsageDegraded::Recovered {
            at: Timestamp(1_700_000_000_000),
        })),
        total: tokens(40),
        rows: rows()
            .iter()
            .enumerate()
            .map(|(i, key)| SessionRow {
                key: key.clone(),
                worktree: (i == 0).then(worktree),
                cumulative: tokens(20),
                session_part: tokens(10),
                shared: i == 0,
            })
            .collect(),
        turns: Arc::from(vec![(turn("t-1"), tokens(15)), (turn("t-2"), tokens(25))]),
    };
    let mr_usage = MrUsage {
        header: header(UsageState::Degraded(UsageDegraded::Unwritable(text(
            "disk full",
        )))),
        mr: mr_label(7, MrState::Merged),
        start: Timestamp(1_000),
        end: Some(Timestamp(4_000)),
        write: tokens(30),
        review: tokens(3),
        shared_with: Arc::from(vec![mr_ref(8)]),
        sessions: Arc::from(vec![MrSessionRow {
            session: session("s-1"),
            tokens: tokens(30),
            turns: Arc::from(vec![(turn("t-1"), tokens(30))]),
        }]),
    };
    let history = HistoryUsage {
        header: header(UsageState::Ready),
        project: info(ProjectStatus::Temporary {
            reason: TemporaryReason::Unborn,
            candidate: None,
        }),
        months: Arc::from(vec![((2024, 1), tokens(1)), ((2026, 10), tokens(2))]),
        days: Some(Arc::from(vec![(date(1), tokens(3))])),
    };
    let history_months_only = HistoryUsage {
        days: None,
        ..history.clone()
    };
    vec![
        UsageSnapshot::Project(project_usage),
        UsageSnapshot::All(all),
        UsageSnapshot::Worktree(worktree_usage),
        UsageSnapshot::Worktree(detached),
        UsageSnapshot::Session(session_usage),
        UsageSnapshot::Mr(mr_usage),
        UsageSnapshot::History(history),
        UsageSnapshot::History(history_months_only),
    ]
}

fn queries() -> Vec<UsageQuery> {
    vec![
        UsageQuery::Project {
            project: ProjectRef::Open(project()),
            window: Window::D7,
        },
        UsageQuery::Project {
            project: ProjectRef::Id(id(0xff)),
            window: Window::D14,
        },
        UsageQuery::AllProjects {
            window: Window::D30,
        },
        UsageQuery::Worktree {
            project: project(),
            worktree: worktree(),
        },
        UsageQuery::Session {
            session: session("s-1"),
        },
        UsageQuery::Mr { mr: mr_ref(7) },
        UsageQuery::History {
            project: ProjectRef::Id(id(1)),
            from: LocalDate {
                year: -1,
                month: 1,
                day: 1,
            },
            to: date(22),
            days: true,
        },
    ]
}

#[test]
fn usage_service_id_and_multiple() {
    assert_eq!(<dyn Usage as Service>::ID, "usage");
    const { assert!(!<dyn Usage as Service>::MULTIPLE) };
    let key = service::<dyn Usage>();
    assert_eq!(key.id(), "usage");
    assert!(!key.multiple());
}

#[test]
fn usage_mr_source_is_not_a_service() {
    // Providers register through `Usage::register_mr_source`; a `MULTIPLE`
    // service would make `usage` wait for one to activate.
    const { assert!(!Probe::<dyn MrSource>::IS_SERVICE) };
    // The probe does tell services apart.
    const { assert!(Probe::<dyn Usage>::IS_SERVICE) };
}

#[test]
fn usage_is_a_shared_trait_object() {
    let usage: Arc<dyn Usage> = Arc::new(Empty);
    let query = UsageQuery::AllProjects { window: Window::D7 };
    let interest = usage.watch(query.clone());
    assert!(usage.snapshot(&query).is_none());
    assert_eq!(*usage.limits(), Limits::default());
    usage.command(UsageCommand::Undo { alias: AliasId(1) });
    let start = AnalysisStart {
        agent: Agent::ClaudeCode,
        at: Timestamp(0),
        target: AnalysisTarget::Mr { mr: mr_ref(7) },
    };
    assert_eq!(usage.begin_analysis(start), Err(UsageError::Unavailable));
    let event = MeterEvent {
        analysis: AnalysisId(1),
        request: 0,
        at: Timestamp(0),
        model: text("model-a"),
        state: MeterState::Forwarded,
    };
    assert_eq!(usage.meter(event), Err(UsageError::UnknownAnalysis));
    drop(interest);
    // `register_mr_source` needs a token from a real activation; its
    // signature is checked by `Empty`, its registration below.

    // Snapshots cross threads inside `Arc`s; registrations and interests go
    // to `PluginHandle::own` or a view's state (`Send`).
    shareable::<Arc<dyn Usage>>();
    shareable::<Arc<dyn MrSource>>();
    shareable::<UsageSnapshot>();
    shareable::<Limits>();
    shareable::<UsageEvent>();
    sendable::<UsageInterest>();
    sendable::<MrSourceRegistration>();
}

#[test]
fn usage_mr_source_is_a_shared_trait_object() {
    let source: Arc<dyn MrSource> = Arc::new(OneMr);
    // Called from an executor thread.
    let s = source.clone();
    std::thread::spawn(move || {
        let repo = s
            .repo("https://forge.test/team/app.git")
            .unwrap()
            .expect("known remote");
        assert_eq!(repo, forge_repo());
        assert_eq!(s.repo("https://other.test/a.git"), Ok(None));
        assert_eq!(
            s.repo("https://offline.test/x.git"),
            Err(MrSourceError::Offline)
        );
        assert_eq!(&*s.mrs_for_branch(&repo, "feat").unwrap(), &[mr_info()]);
        assert!(s.mrs_for_branch(&repo, "main").unwrap().is_empty());
        assert_eq!(&*s.mrs_containing(&repo, &oid(3)).unwrap(), &[mr_info()]);
        assert_eq!(
            s.mrs_containing(&repo, &oid(9)),
            Err(MrSourceError::NotFound)
        );
    })
    .join()
    .unwrap();
}

#[test]
fn usage_interest_releases_once_on_drop() {
    let count = Arc::new(AtomicUsize::new(0));
    let c = count.clone();
    let interest = UsageInterest::new(move || {
        c.fetch_add(1, Ordering::SeqCst);
    });
    assert_eq!(count.load(Ordering::SeqCst), 0);
    assert_eq!(format!("{interest:?}"), "UsageInterest { .. }");
    // Dropped on another thread, as a view's state may be.
    std::thread::spawn(move || drop(interest)).join().unwrap();
    assert_eq!(count.load(Ordering::SeqCst), 1);
}

#[test]
fn usage_mr_source_registration_releases_once_on_drop() {
    let count = Arc::new(AtomicUsize::new(0));
    let c = count.clone();
    let registration = MrSourceRegistration::new(move || {
        c.fetch_add(1, Ordering::SeqCst);
    });
    assert_eq!(count.load(Ordering::SeqCst), 0);
    assert_eq!(format!("{registration:?}"), "MrSourceRegistration { .. }");
    // Dropped on another thread, as a plugin's owned values may be.
    std::thread::spawn(move || drop(registration))
        .join()
        .unwrap();
    assert_eq!(count.load(Ordering::SeqCst), 1);
}

#[test]
fn usage_tokens_total_sums_the_four_kinds() {
    assert_eq!(Tokens::default().total(), 0);
    assert_eq!(tokens(10).total(), 10 + 11 + 12 + 13);
    let huge = Tokens {
        input: u64::MAX,
        output: 1,
        cache_read: 0,
        cache_write: 0,
    };
    assert_eq!(huge.total(), u64::MAX);
}

#[test]
fn usage_event_round_trips() {
    fn event<E: Event + Clone>() {}
    event::<UsageEvent>();

    let mut events: Vec<UsageEvent> = queries()
        .into_iter()
        .enumerate()
        .map(|(i, query)| UsageEvent::Changed {
            query,
            generation: i as u64,
        })
        .collect();
    events.push(UsageEvent::Limits {
        generation: u64::MAX,
    });
    for e in events {
        assert_eq!(round_trip(&e), e);
    }
}

#[test]
fn usage_queries_and_commands_round_trip() {
    for q in queries() {
        assert_eq!(round_trip(&q), q);
    }
    let commands = [
        UsageCommand::Merge {
            from: id(1),
            into: id(2),
        },
        UsageCommand::ConfirmSame {
            temporary: id(3),
            into: id(2),
        },
        UsageCommand::ConfirmNew { temporary: id(3) },
        UsageCommand::Undo {
            alias: AliasId(u64::MAX),
        },
    ];
    for c in commands {
        assert_eq!(round_trip(&c), c);
    }
}

#[test]
fn usage_snapshots_round_trip() {
    for s in snapshots() {
        assert_eq!(round_trip(&s), s);
    }
    // The remaining project status and temporary reasons.
    for reason in [
        TemporaryReason::LocalRewrite,
        TemporaryReason::PrimaryRemoteChanged,
        TemporaryReason::ForgeIdPending,
        TemporaryReason::Unborn,
    ] {
        let i = info(ProjectStatus::Temporary {
            reason,
            candidate: None,
        });
        assert_eq!(round_trip(&i), i);
    }
}

#[test]
fn usage_limits_round_trip() {
    let empty = Limits::default();
    assert_eq!(round_trip(&empty), empty);
    assert!(empty.windows.is_empty());
    assert_eq!(empty.tightest, None);

    let kinds = [
        (Agent::Codex, LimitSource::SessionLog, LimitKind::Primary),
        (Agent::Codex, LimitSource::SessionLog, LimitKind::Secondary),
        (
            Agent::ClaudeCode,
            LimitSource::Statusline,
            LimitKind::FiveHour,
        ),
        (
            Agent::ClaudeCode,
            LimitSource::Statusline,
            LimitKind::SevenDay,
        ),
    ];
    let windows: Vec<LimitWindow> = kinds
        .into_iter()
        .enumerate()
        .map(|(i, (agent, source, kind))| LimitWindow {
            limit_id: (source == LimitSource::SessionLog).then(|| text("codex")),
            window_minutes: (i == 0).then_some(300),
            used_percent: [0.0, 12.5, 99.75, 100.0][i],
            resets_at: (i != 3).then_some(Timestamp(5_000)),
            sampled_at: Timestamp(4_000),
            expired: i == 3,
            agent,
            source,
            kind,
        })
        .collect();
    let limits = Limits {
        generation: 2,
        windows: Arc::from(windows),
        tightest: Some(100),
    };
    assert_eq!(round_trip(&limits), limits);
}

#[test]
fn usage_metering_round_trips() {
    let starts = [
        AnalysisTarget::Session {
            session: session("s-1"),
            turns: Some(Arc::from(vec![turn("t-1"), turn("t-2")])),
        },
        AnalysisTarget::Session {
            session: session("s-2"),
            turns: None,
        },
        AnalysisTarget::Worktree {
            project: project(),
            worktree: worktree(),
        },
        AnalysisTarget::Mr { mr: mr_ref(7) },
    ]
    .map(|target| AnalysisStart {
        agent: Agent::Codex,
        at: Timestamp(1),
        target,
    });
    for s in starts {
        assert_eq!(round_trip(&s), s);
    }
    assert_eq!(round_trip(&AnalysisId(u64::MAX)), AnalysisId(u64::MAX));

    let states = [
        MeterState::Prepared { upper: anthropic() },
        MeterState::Forwarded,
        MeterState::Accepted,
        MeterState::Usage {
            tokens: openai(),
            done: false,
        },
        MeterState::Usage {
            tokens: anthropic(),
            done: true,
        },
        MeterState::Rejected { status: 429 },
    ];
    for (i, state) in states.into_iter().enumerate() {
        let event = MeterEvent {
            analysis: AnalysisId(3),
            request: i as u32,
            at: Timestamp(10 + i as i64),
            model: text("model-a"),
            state,
        };
        assert_eq!(round_trip(&event), event);
    }

    for e in [
        UsageError::Unavailable,
        UsageError::Io(text("no space left on device")),
        UsageError::UnknownAnalysis,
    ] {
        assert_eq!(round_trip(&e), e);
    }
}

#[test]
fn usage_mr_types_round_trip() {
    let info = mr_info();
    assert_eq!(round_trip(&info), info);
    let open = MrInfo {
        merged_target: None,
        bases: Arc::from(Vec::new()),
        timeline: Arc::from(vec![(Timestamp(1), MrEventKind::Opened)]),
        ..mr_info()
    };
    assert_eq!(round_trip(&open), open);
    for e in [
        MrSourceError::Offline,
        MrSourceError::NotFound,
        MrSourceError::Other(text("rate limited")),
    ] {
        assert_eq!(round_trip(&e), e);
    }
}

#[test]
fn usage_enums_are_externally_tagged() {
    // The process plugin protocol relies on serde's default enum tagging.
    let json = serde_json::to_value(UsageQuery::AllProjects { window: Window::D7 }).unwrap();
    assert_eq!(json, serde_json::json!({"AllProjects": {"window": "D7"}}));
    let json = serde_json::to_value(UsageError::Io(text("x"))).unwrap();
    assert_eq!(json, serde_json::json!({"Io": "x"}));
}
