//! `store_*`: units of `code_intel.tags` written and read back, the
//! existence check, lookups by name, the version filter, eviction and its
//! generation, queued access marking, and dropped writes.

use std::path::Path;
use std::time::{Duration, SystemTime};

use lectern_cache::testing::ManualClock;
use lectern_cache::{Budgets, Cache, Config, Param, Partition, Tables, Unit, Written};
use lectern_services::project::BlobId;
use tempfile::TempDir;

use super::extract::run;
use crate::extract::{Def, ExtractStatus, Extraction, LanguageTable, Visibility};
use crate::store::{
    self, EXISTS, EXISTS_BATCH, Found, NAMESPACE, PendingTouches, SCHEMA, STATEMENTS, Store,
    StoredUnit, TOUCH_CHUNK, Version, unit_bytes,
};
use crate::testing::FIXTURES;
use crate::{DefKind, lang_name};

const COUNT_EXT: &str = "SELECT count(*) FROM ci_ext WHERE unit = ?1";
const COUNT_DEF: &str = "SELECT count(*) FROM ci_def WHERE unit = ?1";
const COUNT_REF: &str = "SELECT count(*) FROM ci_ref WHERE unit = ?1";

/// The store's statements plus row counts, for checking what eviction left.
static TEST_STATEMENTS: &[&str] = &[
    store::INSERT_EXT,
    store::INSERT_DEF,
    store::INSERT_REF,
    store::EXISTS,
    store::DEFS_BY_NAME,
    store::REFS_BY_NAME,
    store::UNITS,
    store::DEF_NAMES,
    store::EXT_OF_UNIT,
    store::DEFS_OF_UNIT,
    store::REFS_OF_UNIT,
    COUNT_EXT,
    COUNT_DEF,
    COUNT_REF,
];

static COUNTING: Tables = Tables {
    namespace: NAMESPACE,
    schema: SCHEMA,
    statements: TEST_STATEMENTS,
    quota: store::QUOTA,
};

/// The same namespace with a 64 KiB quota.
static SMALL_QUOTA: Tables = Tables {
    namespace: NAMESPACE,
    schema: SCHEMA,
    statements: STATEMENTS,
    quota: 64 << 10,
};

struct Fixture {
    _dir: TempDir,
    cache: Cache,
    clock: ManualClock,
}

impl Fixture {
    fn new() -> Fixture {
        let dir = tempfile::tempdir().expect("temp dir");
        let clock = ManualClock::new(SystemTime::now());
        let cache = Cache::with_clock(
            Config {
                root: dir.path().join("cache"),
                budgets: Budgets::default(),
                ui_thread: None,
            },
            clock.clone(),
        );
        Fixture {
            _dir: dir,
            cache,
            clock,
        }
    }

    fn partition(&self) -> Partition {
        self.cache.repo(Path::new("/work/repo/.git"))
    }

    fn store(&self) -> Store {
        Store::new(&self.partition())
    }
}

fn oid(i: u32) -> BlobId {
    let mut b = [0u8; 20];
    b[..4].copy_from_slice(&i.to_be_bytes());
    b[19] = 0xa5;
    BlobId::Sha1(b)
}

fn v(lang: &str, grammar: u64, query: u64) -> Version {
    Version::new(lang_name(lang), grammar, query)
}

fn def(name: &str, kind: DefKind, line: u32) -> Def {
    Def {
        name: name.into(),
        kind,
        line,
        column: 4,
        end_line: line + 2,
        scope: None,
        visibility: Visibility::Public,
        arity: None,
    }
}

fn done(defs: Vec<Def>, refs: &[&str]) -> Extraction {
    let mut refs: Vec<Box<str>> = refs.iter().map(|r| Box::from(*r)).collect();
    refs.sort();
    refs.dedup();
    Extraction {
        status: ExtractStatus::Done,
        defs,
        refs,
    }
}

/// About 1 KiB declared: ten definitions with 20-byte names.
fn sized(i: u32) -> Extraction {
    let defs = (0..10)
        .map(|j| def(&format!("unit{i:05}_symbol{j:06}"), DefKind::Function, j))
        .collect();
    done(defs, &[])
}

fn commit(store: &Store, units: &[(BlobId, &Version, &Extraction)]) {
    let mut w = store.write();
    for (oid, version, extraction) in units {
        assert!(w.add(*oid, version, extraction));
    }
    assert_eq!(w.commit(), Written::Committed);
}

fn found_oids(found: &[Found]) -> Vec<BlobId> {
    let mut oids: Vec<BlobId> = found.iter().map(|f| f.oid).collect();
    oids.sort();
    oids
}

fn unit_oids(units: &[StoredUnit]) -> Vec<(BlobId, String)> {
    let mut out: Vec<_> = units
        .iter()
        .map(|u| (u.oid, u.lang.0.to_string()))
        .collect();
    out.sort();
    out
}

/// Rows of `unit` in `ci_ext`, `ci_def`, `ci_ref`.
fn rows(p: &Partition, unit: Unit) -> [i64; 3] {
    let set = p.tables(&COUNTING);
    set.read(|r| {
        let mut counts = [0; 3];
        for (i, sql) in [COUNT_EXT, COUNT_DEF, COUNT_REF].into_iter().enumerate() {
            assert!(r.query(sql, &[Param::Int(unit.id())], |row| {
                counts[i] = row.int(0).expect("count");
                false
            }));
        }
        counts
    })
    .expect("read")
}

#[test]
fn store_exists_sql_has_a_static_in_of_500() {
    assert_eq!(EXISTS_BATCH, 500);
    assert_eq!(EXISTS.matches('?').count(), 3 + EXISTS_BATCH);
    assert!(EXISTS.ends_with("?,?)"));
    assert!(
        STATEMENTS.len() <= 32,
        "the cache keeps 32 statements per connection"
    );
}

#[test]
fn store_write_and_read_back_fixtures() {
    let f = Fixture::new();
    let store = f.store();
    let table = LanguageTable::core();
    let mut written = Vec::new();
    for (i, (lang, name, source)) in FIXTURES.iter().enumerate() {
        let extraction = run(lang, source);
        assert_eq!(extraction.status, ExtractStatus::Done, "{name}");
        let version = Version::of(table.get(lang).expect("core language"));
        commit(&store, &[(oid(i as u32), &version, &extraction)]);
        written.push((oid(i as u32), version, extraction));
    }
    for (oid, version, extraction) in &written {
        let found = store.exists(version, &[*oid]).expect("read");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].status, ExtractStatus::Done);
        let back = store.load(found[0].unit).expect("stored");
        assert_eq!(&back, extraction, "{}", version.lang.0);
    }
}

#[test]
fn store_write_and_read_back_every_field() {
    let f = Fixture::new();
    let store = f.store();
    let rust = v("rust", 7, 9);
    let mut a = def("Queue", DefKind::Struct, 0);
    a.visibility = Visibility::Module;
    let mut b = def("push", DefKind::Method, 5);
    b.scope = Some("Queue".into());
    b.arity = Some(255);
    b.visibility = Visibility::Package;
    let mut c = def("LIMIT", DefKind::Constant, 12);
    c.visibility = Visibility::Unknown;
    let mut d = def("helper", DefKind::Other, 20);
    d.visibility = Visibility::File;
    d.arity = Some(0);
    let extraction = done(vec![a, b, c, d], &["Vec", "len", "push"]);
    let timed_out = Extraction {
        status: ExtractStatus::TimedOut,
        defs: vec![def("ignored", DefKind::Function, 0)],
        refs: vec!["ignored".into()],
    };
    let mut w = store.write();
    assert!(w.add(oid(1), &rust, &extraction));
    assert!(w.add(oid(2), &rust, &timed_out));
    for status in [ExtractStatus::Cancelled, ExtractStatus::Unsupported] {
        let not_stored = Extraction {
            status,
            defs: Vec::new(),
            refs: Vec::new(),
        };
        assert!(!w.add(oid(3), &rust, &not_stored));
    }
    assert_eq!(w.len(), 2);
    assert_eq!(w.commit(), Written::Committed);

    let found = store
        .exists(&rust, &[oid(1), oid(2), oid(3)])
        .expect("read");
    assert_eq!(found_oids(&found), vec![oid(1), oid(2)]);
    let unit = |o: BlobId| found.iter().find(|f| f.oid == o).expect("found");
    assert_eq!(store.load(unit(oid(1)).unit), Some(extraction));
    // Not Done: the ci_ext row only, so it is not retried, and no lookup
    // returns it.
    assert_eq!(unit(oid(2)).status, ExtractStatus::TimedOut);
    let back = store.load(unit(oid(2)).unit).expect("stored");
    assert_eq!(back.status, ExtractStatus::TimedOut);
    assert!(back.defs.is_empty() && back.refs.is_empty());
    assert_eq!(rows(&f.partition(), unit(oid(2)).unit), [1, 0, 0]);
    assert_eq!(
        store.defs_by_name("ignored", std::slice::from_ref(&rust)),
        Some(vec![])
    );
    assert_eq!(
        store.refs_by_name("ignored", std::slice::from_ref(&rust)),
        Some(vec![])
    );
    assert_eq!(store.units(&[rust]).map(|u| u.len()), Some(1));
}

#[test]
fn store_unit_bytes_follow_the_estimate() {
    let mut a = def("abc", DefKind::Function, 0);
    a.scope = Some("Outer".into());
    let extraction = done(vec![a, def("de", DefKind::Class, 1)], &["x", "yz"]);
    assert_eq!(
        unit_bytes(&extraction),
        64 + (2 * 3 + 5 + 40) + (2 * 2 + 40) + (2 + 24) + (4 + 24)
    );
    let timed_out = Extraction {
        status: ExtractStatus::TimedOut,
        ..extraction
    };
    assert_eq!(unit_bytes(&timed_out), 64);
}

#[test]
fn store_exists_matches_the_whole_key() {
    let f = Fixture::new();
    let store = f.store();
    let current = v("rust", 1, 1);
    let extraction = done(vec![def("f", DefKind::Function, 0)], &[]);
    commit(&store, &[(oid(1), &current, &extraction)]);
    let sha256 = BlobId::Sha256([7; 32]);
    commit(&store, &[(sha256, &current, &extraction)]);

    let found = store
        .exists(&current, &[oid(1), oid(2), sha256])
        .expect("read");
    assert_eq!(found_oids(&found), vec![oid(1), sha256]);
    for other in [v("js", 1, 1), v("rust", 2, 1), v("rust", 1, 2)] {
        assert_eq!(
            store.exists(&other, &[oid(1), sha256]),
            Some(vec![]),
            "{other:?}"
        );
    }
    // A 20-byte prefix of a 32-byte OID is another OID.
    let mut prefix = [0u8; 20];
    prefix.copy_from_slice(&[7; 20]);
    assert_eq!(
        store.exists(&current, &[BlobId::Sha1(prefix)]),
        Some(vec![])
    );
    assert_eq!(store.exists(&current, &[]), Some(vec![]));
}

#[test]
fn store_exists_spans_several_queries() {
    let f = Fixture::new();
    let store = f.store();
    let ts = v("ts", 3, 4);
    let extraction = done(vec![def("f", DefKind::Function, 0)], &["g"]);
    let mut w = store.write();
    for i in (0..1300).filter(|i| i % 3 != 0) {
        assert!(w.add(oid(i), &ts, &extraction));
    }
    assert_eq!(w.commit(), Written::Committed);
    let oids: Vec<BlobId> = (0..1300).map(oid).collect();
    let found = store.exists(&ts, &oids).expect("read");
    let expected: Vec<BlobId> = (0..1300).filter(|i| i % 3 != 0).map(oid).collect();
    assert_eq!(found_oids(&found), expected);
}

#[test]
fn store_lookups_by_name_are_case_sensitive() {
    let f = Fixture::new();
    let store = f.store();
    let js = v("js", 1, 1);
    let a = done(
        vec![
            def("handle", DefKind::Function, 3),
            def("Handle", DefKind::Class, 9),
        ],
        &["HANDLE", "handle"],
    );
    let b = done(vec![def("HANDLE", DefKind::Constant, 1)], &["Handle"]);
    commit(&store, &[(oid(1), &js, &a), (oid(2), &js, &b)]);
    let versions = [js];

    let defs = store.defs_by_name("handle", &versions).expect("read");
    assert_eq!(defs.len(), 1);
    assert_eq!(defs[0].oid, oid(1));
    assert_eq!(&*defs[0].lang.0, "js");
    assert_eq!(defs[0].def, def("handle", DefKind::Function, 3));
    let defs = store.defs_by_name("HANDLE", &versions).expect("read");
    assert_eq!(defs.iter().map(|d| d.oid).collect::<Vec<_>>(), vec![oid(2)]);
    assert_eq!(store.defs_by_name("hAnDlE", &versions), Some(vec![]));

    let refs = store.refs_by_name("Handle", &versions).expect("read");
    assert_eq!(unit_oids(&refs), vec![(oid(2), "js".into())]);
    let refs = store.refs_by_name("handle", &versions).expect("read");
    assert_eq!(unit_oids(&refs), vec![(oid(1), "js".into())]);
    assert_eq!(store.refs_by_name("hANDLE", &versions), Some(vec![]));
}

#[test]
fn store_lookups_cover_a_compatibility_group() {
    let f = Fixture::new();
    let store = f.store();
    // Five languages: more than one query's four triples.
    let langs = ["ts", "tsx", "js", "kotlin", "java"];
    let versions: Vec<Version> = langs.iter().map(|l| v(l, 2, 2)).collect();
    let extraction = done(vec![def("render", DefKind::Function, 0)], &["render"]);
    let units: Vec<_> = versions
        .iter()
        .enumerate()
        .map(|(i, version)| (oid(i as u32), version, &extraction))
        .collect();
    commit(&store, &units);
    let rust = v("rust", 2, 2);
    commit(&store, &[(oid(99), &rust, &extraction)]);

    let expected: Vec<(BlobId, String)> = langs
        .iter()
        .enumerate()
        .map(|(i, l)| (oid(i as u32), l.to_string()))
        .collect();
    let mut defs: Vec<_> = store
        .defs_by_name("render", &versions)
        .expect("read")
        .into_iter()
        .map(|d| (d.oid, d.lang.0.to_string()))
        .collect();
    defs.sort();
    assert_eq!(defs, expected);
    let refs = store.refs_by_name("render", &versions).expect("read");
    assert_eq!(unit_oids(&refs), expected);
    let all = store.units(&versions).expect("read");
    assert_eq!(unit_oids(&all), expected);
}

/// The same OIDs under an old grammar version and an old query version:
/// only the current triple's units come back.
#[test]
fn store_version_filter_returns_current_units_only() {
    let f = Fixture::new();
    let store = f.store();
    let current = v("go", 5, 8);
    let old_grammar = v("go", 4, 8);
    let old_query = v("go", 5, 7);
    let old = done(vec![def("Old", DefKind::Type, 0)], &["Old", "Shared"]);
    let new = done(vec![def("New", DefKind::Type, 2)], &["New", "Shared"]);
    let shared = done(vec![def("Shared", DefKind::Function, 4)], &["Shared"]);
    commit(
        &store,
        &[
            (oid(1), &old_grammar, &old),
            (oid(1), &old_query, &old),
            (oid(2), &old_grammar, &shared),
            (oid(1), &current, &new),
            (oid(3), &current, &shared),
        ],
    );
    let versions = [current.clone()];

    // Existence check.
    let found = store
        .exists(&current, &[oid(1), oid(2), oid(3)])
        .expect("read");
    assert_eq!(found_oids(&found), vec![oid(1), oid(3)]);
    assert_eq!(
        found_oids(
            &store
                .exists(&old_grammar, &[oid(1), oid(2), oid(3)])
                .expect("read")
        ),
        vec![oid(1), oid(2)]
    );
    // Definitions by name.
    assert_eq!(store.defs_by_name("Old", &versions), Some(vec![]));
    let shared_defs = store.defs_by_name("Shared", &versions).expect("read");
    assert_eq!(
        shared_defs.iter().map(|d| d.oid).collect::<Vec<_>>(),
        vec![oid(3)]
    );
    let new_defs = store.defs_by_name("New", &versions).expect("read");
    assert_eq!(
        new_defs.iter().map(|d| d.oid).collect::<Vec<_>>(),
        vec![oid(1)]
    );
    // References by name.
    assert_eq!(store.refs_by_name("Old", &versions), Some(vec![]));
    let refs = store.refs_by_name("Shared", &versions).expect("read");
    assert_eq!(
        unit_oids(&refs),
        vec![(oid(1), "go".into()), (oid(3), "go".into())]
    );
    // The name table's units and names.
    let units = store.units(&versions).expect("read");
    assert_eq!(
        unit_oids(&units),
        vec![(oid(1), "go".into()), (oid(3), "go".into())]
    );
    let mut ids: Vec<Unit> = units.iter().map(|u| u.unit).collect();
    ids.sort();
    let mut names = Vec::new();
    assert!(store.def_names(&ids, |_, name, kind| names.push((name.to_string(), kind))));
    names.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(
        names,
        vec![
            ("New".to_string(), DefKind::Type),
            ("Shared".to_string(), DefKind::Function)
        ]
    );
}

#[test]
fn store_rewriting_a_key_replaces_its_unit() {
    let f = Fixture::new();
    let store = f.store();
    let py = v("python", 1, 1);
    let first = done(vec![def("first", DefKind::Function, 0)], &["a"]);
    let second = done(vec![def("second", DefKind::Function, 0)], &["b"]);
    commit(&store, &[(oid(1), &py, &first)]);
    let before = store.evictions();
    commit(&store, &[(oid(1), &py, &second)]);
    // The old unit was deleted: the generation moves.
    assert!(store.evictions() > before);
    let found = store.exists(&py, &[oid(1)]).expect("read");
    assert_eq!(found.len(), 1);
    assert_eq!(store.load(found[0].unit), Some(second));
    assert_eq!(
        store.defs_by_name("first", std::slice::from_ref(&py)),
        Some(vec![])
    );
    assert_eq!(store.refs_by_name("a", &[py]), Some(vec![]));
}

/// Over the quota, the namespace's own writes evict its units by access
/// time; the existence check's touch keeps a unit; evicted units leave no
/// row in the three tables; the eviction generation moves only then.
#[test]
fn store_quota_evicts_by_access_time() {
    let f = Fixture::new();
    let p = f.partition();
    let store = Store::with_tables(&p, &SMALL_QUOTA);
    let other = Store::new(&p);
    let rust = v("rust", 1, 1);
    let extractions: Vec<Extraction> = (0..70).map(sized).collect();
    let generation = store.evictions();

    // 40 units of about 1 KiB: within the 64 KiB quota.
    let mut w = store.write();
    for i in 0..40 {
        assert!(w.add(oid(i), &rust, &extractions[i as usize]));
    }
    assert_eq!(w.commit(), Written::Committed);
    let old: Vec<BlobId> = (0..40).map(oid).collect();
    let units: Vec<Found> = store.exists(&rust, &old).expect("read");
    assert_eq!(units.len(), 40);
    // Reads and writes of new units leave the generation alone.
    assert_eq!(store.evictions(), generation);

    // Two hours later a warm pass finds all 40, but only the hits of the
    // first 20 are queued and marked afterwards. The existence check itself
    // marks nothing: if it did, all 40 would look equally recent and
    // eviction would take the lowest IDs, units 0–13.
    f.clock.advance(Duration::from_secs(2 * 3600));
    let generation_before = store.evictions();
    let seen = store.exists(&rust, &old).expect("read");
    assert_eq!(seen.len(), 40);
    let first: Vec<Found> = seen
        .into_iter()
        .filter(|f| (0..20).map(oid).any(|o| o == f.oid))
        .collect();
    assert_eq!(first.len(), 20);
    let mut pending = PendingTouches::new();
    pending.push(generation_before, &first);
    assert_eq!(store.touch_pending(&mut pending), 0);
    assert!(pending.is_empty());
    assert_eq!(store.evictions(), generation);

    // 30 more units go over the quota.
    let mut w = store.write();
    for i in 40..70 {
        assert!(w.add(oid(i), &rust, &extractions[i as usize]));
    }
    assert_eq!(w.commit(), Written::Committed);
    assert!(store.evictions() > generation);
    // Both handles of the namespace share the generation.
    assert_eq!(other.evictions(), store.evictions());

    let all: Vec<BlobId> = (0..70).map(oid).collect();
    let left: Vec<BlobId> = found_oids(&store.exists(&rust, &all).expect("read"));
    let evicted: Vec<&Found> = units.iter().filter(|u| !left.contains(&u.oid)).collect();
    assert!(!evicted.is_empty(), "nothing was evicted");
    for i in (0..20).chain(40..70) {
        assert!(left.contains(&oid(i)), "unit {i} was evicted");
    }
    for unit in evicted {
        assert_eq!(rows(&p, unit.unit), [0, 0, 0], "{:?}", unit.oid);
        let i = u32::from_be_bytes(unit.oid.as_bytes()[..4].try_into().expect("4 bytes"));
        let name = &extractions[i as usize].defs[0].name;
        assert_eq!(
            store.defs_by_name(name, std::slice::from_ref(&rust)),
            Some(vec![])
        );
    }
    // A kept unit still has its rows.
    let kept = units.iter().find(|u| u.oid == oid(0)).expect("unit 0");
    assert_eq!(rows(&p, kept.unit), [1, 10, 0]);
}

/// Queued hits are marked `TOUCH_CHUNK` at a time; a queue read under an
/// older eviction generation is dropped, since its IDs may have been reused.
#[test]
fn store_pending_touches_drain_in_chunks() {
    let f = Fixture::new();
    let store = f.store();
    let rust = v("rust", 1, 1);
    let small = done(vec![def("f", DefKind::Function, 0)], &[]);
    let mut w = store.write();
    for i in 0..2500 {
        assert!(w.add(oid(i), &rust, &small));
    }
    assert_eq!(w.commit(), Written::Committed);
    let oids: Vec<BlobId> = (0..2500).map(oid).collect();

    let generation = store.evictions();
    let found = store.exists(&rust, &oids).expect("read");
    let mut pending = PendingTouches::new();
    pending.push(generation, &found);
    assert_eq!(pending.len(), 2500);
    assert_eq!(store.touch_pending(&mut pending), 2500 - TOUCH_CHUNK);
    assert_eq!(store.touch_pending(&mut pending), 2500 - 2 * TOUCH_CHUNK);
    assert_eq!(store.touch_pending(&mut pending), 0);
    assert_eq!(store.touch_pending(&mut pending), 0);

    // A unit is replaced: the generation moves, and the queue read before it
    // is dropped without marking anything.
    pending.push(generation, &found);
    commit(&store, &[(oid(0), &rust, &small)]);
    assert_ne!(store.evictions(), generation);
    assert_eq!(store.touch_pending(&mut pending), 0);
    assert!(pending.is_empty());
    // Hits of the new generation replace a queue of the old one.
    pending.push(generation, &found[..10]);
    let now = store.evictions();
    let fresh = store.exists(&rust, &oids[..3]).expect("read");
    pending.push(now, &fresh);
    assert_eq!(pending.len(), 3);
    assert_eq!(store.touch_pending(&mut pending), 0);
}

#[test]
fn store_dropped_write_does_not_panic() {
    let f = Fixture::new();
    let p = f.partition();
    let store = Store::new(&p);
    let rust = v("rust", 1, 1);
    // Creates the tables, then keeps the file at its size.
    assert_eq!(store.units(std::slice::from_ref(&rust)), Some(vec![]));
    p.set_max_page_count(Some(1));
    let extractions: Vec<Extraction> = (0..200).map(sized).collect();
    let mut w = store.write();
    for (i, extraction) in extractions.iter().enumerate() {
        assert!(w.add(oid(i as u32), &rust, extraction));
    }
    let generation = store.evictions();
    assert!(matches!(w.commit(), Written::Dropped(_)));
    // Nothing was written; the extractions are still the caller's.
    assert_eq!(store.exists(&rust, &[oid(0), oid(199)]), Some(vec![]));
    assert_eq!(extractions[199].defs.len(), 10);
    assert_eq!(store.evictions(), generation);
    // Writes commit again once there is room (after the 60 s in which they
    // are dropped without trying). The unit may be evicted at once: a full
    // disk lowered the partition's budget below its size.
    p.set_max_page_count(None);
    f.clock.advance(Duration::from_secs(61));
    commit(&store, &[(oid(0), &rust, &extractions[0])]);
}
