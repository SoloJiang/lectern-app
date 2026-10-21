use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use lectern_kernel::{ActivationToken, Event, Service, service};

use super::*;
use crate::project::BlobId;

/// A `CodeIntel` that knows nothing, to check the trait is usable as a
/// shared trait object.
struct Empty;

impl CodeIntel for Empty {
    fn set_focus(&self, _project: &ProjectKey, _worktree: Option<&WorktreeKey>, _focus: Focus) {}
    fn query(&self, _query: Query) -> QueryId {
        QueryId(1)
    }
    fn results(&self, _id: QueryId) -> Option<Arc<QueryResults>> {
        None
    }
    fn close(&self, _id: QueryId) {}
    fn prefetch(&self, _query: Query) {}
    fn names(&self, _project: &ProjectKey) -> Names {
        Names::Unavailable
    }
    fn register_provider(
        &self,
        _provider: Arc<dyn NavigationProvider>,
        _token: ActivationToken,
    ) -> ProviderRegistration {
        ProviderRegistration::new(|| {})
    }
    fn coverage(&self, _project: &ProjectKey) -> Option<Coverage> {
        None
    }
}

/// A provider that answers every request with no locations and records
/// cancellations.
#[derive(Default)]
struct Silent {
    cancelled: Mutex<Vec<SemanticRequestId>>,
}

impl NavigationProvider for Silent {
    fn id(&self) -> &str {
        "silent"
    }
    fn handles(&self, lang: &LangName) -> bool {
        &*lang.0 == "java"
    }
    fn head_only(&self) -> bool {
        false
    }
    fn definition(&self, _request: SemanticRequest, reply: SemanticReply) {
        reply.send(Ok(Arc::from(Vec::new())));
    }
    fn references(&self, _request: SemanticRequest, reply: SemanticReply) {
        reply.send(Err(SemanticError::Unavailable));
    }
    fn cancel(&self, request: SemanticRequestId) {
        self.cancelled.lock().unwrap().push(request);
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
    WorktreeKey(path("/r/.git"))
}

fn oid(byte: u8) -> BlobId {
    BlobId::Sha1([byte; 20])
}

fn java() -> LangName {
    LangName(text("java"))
}

fn text_ref(origin: TextOrigin) -> TextRef {
    TextRef {
        worktree: Some(worktree()),
        path: text("src/A.java"),
        oid: oid(0xa1),
        origin,
    }
}

fn location(p: &str, line: u32, origin: TextOrigin) -> Location {
    Location {
        worktree: Some(worktree()),
        path: text(p),
        origin,
        oid: BlobId::Sha256([0x5e; 32]),
        line,
        column: 4,
        end_line: line + 2,
    }
}

fn definition() -> DefinitionRef {
    DefinitionRef {
        location: location("src/B.java", 10, TextOrigin::Worktree),
        name: text("run"),
        kind: DefKind::Method,
        lang: java(),
        scope: Some(text("B")),
        local: false,
    }
}

fn request(id: u64) -> SemanticRequest {
    SemanticRequest {
        id: SemanticRequestId(id),
        worktree_root: path("/r"),
        at: text_ref(TextOrigin::Worktree),
        position: Position { line: 3, column: 8 },
        head_clean: true,
    }
}

#[test]
fn code_intel_service_id_and_multiple() {
    assert_eq!(<dyn CodeIntel as Service>::ID, "code-intel");
    const { assert!(!<dyn CodeIntel as Service>::MULTIPLE) };
    let key = service::<dyn CodeIntel>();
    assert_eq!(key.id(), "code-intel");
    assert!(!key.multiple());
}

#[test]
fn code_intel_is_a_shared_trait_object() {
    let intel: Arc<dyn CodeIntel> = Arc::new(Empty);
    intel.set_focus(&project(), Some(&worktree()), Focus::Current(None));
    let query = Query {
        project: project(),
        kind: QueryKind::Named {
            name: text("run"),
            near: None,
        },
    };
    intel.prefetch(query.clone());
    let id = intel.query(query);
    assert!(intel.results(id).is_none());
    intel.close(id);
    assert_eq!(intel.names(&project()), Names::Unavailable);
    assert!(intel.coverage(&project()).is_none());
    // `register_provider` needs a token from a real activation; its
    // signature is checked by `Empty`, its registration below.

    // Results and tables cross threads inside `Arc`s; registrations go to
    // `PluginHandle::own` (`Send`), replies to any thread.
    shareable::<Arc<dyn CodeIntel>>();
    shareable::<Arc<dyn NavigationProvider>>();
    shareable::<QueryResults>();
    shareable::<NameTable>();
    shareable::<CodeIntelEvent>();
    sendable::<ProviderRegistration>();
    sendable::<SemanticReply>();
}

#[test]
fn code_intel_event_round_trips() {
    fn event<E: Event + Clone>() {}
    event::<CodeIntelEvent>();

    let mut table = NameTable::builder(4);
    table.push("run", 1 << DefKind::Method as u16);
    let events = [
        CodeIntelEvent::Results {
            id: QueryId(7),
            generation: 2,
        },
        CodeIntelEvent::Names {
            project: project(),
            generation: 4,
            table: Arc::new(table.build()),
        },
    ];
    for event in events {
        assert_eq!(round_trip(&event), event);
    }
}

#[test]
fn code_intel_queries_and_focus_round_trip() {
    let queries = [
        QueryKind::Definition {
            at: text_ref(TextOrigin::Worktree),
            position: Position { line: 0, column: 0 },
        },
        QueryKind::Definition {
            at: text_ref(TextOrigin::Object),
            position: Position {
                line: 12,
                column: 30,
            },
        },
        QueryKind::References { of: definition() },
        QueryKind::Named {
            name: text("Meters"),
            near: Some(text("pkg/units")),
        },
    ];
    for kind in queries {
        let query = Query {
            project: ProjectKey::Folder { root: path("/f") },
            kind,
        };
        assert_eq!(round_trip(&query), query);
    }

    let files: Arc<[RelPath]> = Arc::from(vec![text("a.ts"), text("b/c.ts")]);
    let focus = [
        Focus::Current(None),
        Focus::Current(Some(TextRef {
            worktree: None,
            ..text_ref(TextOrigin::Worktree)
        })),
        Focus::Drawer(files.clone()),
        Focus::Review(files.clone()),
        Focus::Touched(Arc::from(Vec::new())),
    ];
    for f in focus {
        assert_eq!(round_trip(&f), f);
    }
}

#[test]
fn code_intel_results_round_trip() {
    let candidate = |tier, evidence, source| Candidate {
        definition: definition(),
        tier,
        evidence,
        in_review: tier == CandidateTier::CurrentFile,
        source,
    };
    let reference = |syntax, enclosing: Option<&str>| Reference {
        location: location("src/C.java", 40, TextOrigin::Object),
        syntax,
        enclosing: enclosing.map(text),
        in_review: false,
    };
    let results = QueryResults {
        id: QueryId(3),
        generation: 5,
        symbol: Some(Symbol {
            name: text("run"),
            lang: java(),
            syntax: RefSyntax::MemberCall,
            defines: Some(definition()),
        }),
        candidates: Arc::from(vec![
            candidate(
                CandidateTier::CurrentFile,
                Evidence::Bound(Binding::Local),
                CandidateSource::Index,
            ),
            candidate(
                CandidateTier::Imported,
                Evidence::Bound(Binding::Import),
                CandidateSource::Resolver,
            ),
            candidate(
                CandidateTier::Elsewhere,
                Evidence::NameOnly,
                CandidateSource::Scanned,
            ),
        ]),
        callers: Arc::from(vec![reference(RefSyntax::Call, Some("main"))]),
        same_name: Arc::from(vec![reference(RefSyntax::Plain, None)]),
        jump: None,
        preselect: Some(0),
        semantic: SemanticState::Ready(Arc::from(vec![location(
            "src/D.java",
            1,
            TextOrigin::Worktree,
        )])),
        state: QueryState::Done(Completeness::Partial {
            unsearched: 12,
            outside: 300,
        }),
    };
    assert_eq!(round_trip(&results), results);

    for semantic in [
        SemanticState::NotRequested,
        SemanticState::Pending,
        SemanticState::Same,
        SemanticState::Failed,
    ] {
        let r = QueryResults {
            semantic,
            ..results.clone()
        };
        assert_eq!(round_trip(&r), r);
    }
    let states = [
        QueryState::Running,
        QueryState::Done(Completeness::Complete),
        QueryState::Failed(QueryError::NotOpen),
        QueryState::Failed(QueryError::UnsupportedLanguage),
        QueryState::Failed(QueryError::NoSymbol),
        QueryState::Failed(QueryError::Stale),
        QueryState::Failed(QueryError::TooLarge),
        QueryState::Failed(QueryError::Unreadable(text("permission denied"))),
    ];
    for state in states {
        assert_eq!(round_trip(&state), state);
    }
    for binding in [Binding::Local, Binding::Import, Binding::QualifiedName] {
        let evidence = Evidence::Bound(binding);
        assert_eq!(round_trip(&evidence), evidence);
    }
    let definition = DefinitionRef {
        scope: None,
        local: true,
        kind: DefKind::Variable,
        ..definition()
    };
    assert_eq!(round_trip(&definition), definition);
}

#[test]
fn code_intel_candidate_tiers_sort_in_listing_order() {
    let mut tiers = vec![
        CandidateTier::Elsewhere,
        CandidateTier::SameSubproject,
        CandidateTier::CurrentFile,
        CandidateTier::DependentSubproject,
        CandidateTier::Imported,
        CandidateTier::SamePackage,
    ];
    tiers.sort();
    assert_eq!(
        tiers,
        [
            CandidateTier::CurrentFile,
            CandidateTier::Imported,
            CandidateTier::SamePackage,
            CandidateTier::SameSubproject,
            CandidateTier::DependentSubproject,
            CandidateTier::Elsewhere,
        ]
    );
}

#[test]
fn code_intel_coverage_and_names_round_trip() {
    let coverage = Coverage {
        generation: 9,
        focus: TierState::Done,
        subproject: TierState::Done,
        worktree: TierState::Pending,
        other_worktrees: TierState::Pending,
    };
    assert_eq!(round_trip(&coverage), coverage);

    let mut table = NameTable::builder(2);
    table.push("b", 1);
    table.push("a", 4);
    let names = [
        Names::Ready(Arc::new(table.build())),
        Names::Building,
        Names::Unavailable,
    ];
    for n in names {
        assert_eq!(round_trip(&n), n);
    }
}

#[test]
fn name_table_sorts_and_merges_kinds() {
    let bit = |k: DefKind| 1u16 << k as u16;
    let mut b = NameTable::builder(11);
    b.push("run", bit(DefKind::Method));
    b.push("Run", bit(DefKind::Class));
    b.push("ärger", bit(DefKind::Function));
    b.push("run", bit(DefKind::Function));
    b.push("", bit(DefKind::Other));
    let table = b.build();
    assert_eq!(table.generation(), 11);
    // Byte order, case-sensitive dedup; the empty name sorts first.
    assert_eq!(
        table.iter().collect::<Vec<_>>(),
        ["", "Run", "run", "ärger"]
    );
    assert_eq!(table.len(), 4);
    assert!(!table.is_empty());
    assert_eq!(table.kinds(1), bit(DefKind::Class));
    assert_eq!(
        table.kinds(2),
        bit(DefKind::Method) | bit(DefKind::Function)
    );
    assert_eq!(table.get(3), Some("ärger"));
    assert_eq!(table.get(4), None);
    assert_eq!(table.kinds(4), 0);
    assert_eq!(
        bit(DefKind::Other),
        1 << 13,
        "bit order is declaration order"
    );

    let empty = NameTable::builder(0).build();
    assert!(empty.is_empty());
    assert_eq!(empty.get(0), None);
}

#[test]
fn name_table_serializes_entries_and_deserializes_through_the_builder() {
    let mut b = NameTable::builder(3);
    b.push("b", 2);
    b.push("a", 1);
    let table = b.build();
    assert_eq!(
        serde_json::to_string(&table).unwrap(),
        r#"{"generation":3,"entries":[["a",1],["b",2]]}"#
    );
    // Unsorted and duplicated entries from another process.
    let json = r#"{"generation":5,"entries":[["z",1],["a",2],["z",4]]}"#;
    let back: NameTable = serde_json::from_str(json).expect("deserializes");
    assert_eq!(back.generation(), 5);
    assert_eq!(back.iter().collect::<Vec<_>>(), ["a", "z"]);
    assert_eq!(back.kinds(1), 5);
}

#[test]
fn semantic_types_round_trip() {
    let req = request(42);
    assert_eq!(round_trip(&req), req);
    let loc = SemanticLocation {
        path: text("src/Impl.java"),
        line: 8,
        column: 2,
        end_line: 20,
    };
    assert_eq!(round_trip(&loc), loc);
    for error in [
        SemanticError::Unavailable,
        SemanticError::Timeout,
        SemanticError::Failed(text("jdtls exited")),
    ] {
        assert_eq!(round_trip(&error), error);
    }
}

#[test]
fn semantic_reply_delivers_once_and_drop_delivers_nothing() {
    let got = Arc::new(Mutex::new(Vec::new()));
    let reply = |got: &Arc<Mutex<Vec<_>>>| {
        let got = got.clone();
        SemanticReply::new(move |result| got.lock().unwrap().push(result))
    };
    let provider: Arc<dyn NavigationProvider> = Arc::new(Silent::default());
    assert_eq!(provider.id(), "silent");
    assert!(provider.handles(&java()));
    assert!(!provider.handles(&LangName(text("go"))));
    assert!(!provider.head_only());

    provider.definition(request(1), reply(&got));
    // Sent from another thread.
    let r = reply(&got);
    std::thread::spawn(move || r.send(Err(SemanticError::Timeout)))
        .join()
        .unwrap();
    provider.references(request(2), reply(&got));
    drop(reply(&got));
    assert_eq!(
        *got.lock().unwrap(),
        [
            Ok(Arc::from(Vec::new())),
            Err(SemanticError::Timeout),
            Err(SemanticError::Unavailable),
        ]
    );
}

#[test]
fn provider_registration_unregisters_once_on_drop() {
    let count = Arc::new(AtomicUsize::new(0));
    let c = count.clone();
    let registration = ProviderRegistration::new(move || {
        c.fetch_add(1, Ordering::SeqCst);
    });
    assert_eq!(count.load(Ordering::SeqCst), 0);
    // Dropped on another thread, as `PluginHandle::own` may.
    std::thread::spawn(move || drop(registration))
        .join()
        .unwrap();
    assert_eq!(count.load(Ordering::SeqCst), 1);

    let silent = Silent::default();
    silent.cancel(SemanticRequestId(9));
    assert_eq!(*silent.cancelled.lock().unwrap(), [SemanticRequestId(9)]);
}
