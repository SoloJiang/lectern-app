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
    self, EXISTS, EXISTS_BATCH, Found, NAMESPACE, NamePager, Part, PendingTouches, SCHEMA,
    STATEMENTS, Store, StoredUnit, TOUCH_CHUNK, UnitMeta, Version, WindowRow, chain_complete,
    unit_bytes,
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
    store::DEFS_PAGE,
    store::DEFS_LEFT,
    store::DEFS_IN,
    store::REFS_PAGE,
    store::REFS_LEFT,
    store::UNITS,
    store::NAMES_PAGE,
    store::EXT_OF_UNIT,
    store::DEFS_OF_UNIT,
    store::REFS_OF_UNIT,
    store::WINDOWS,
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
        byte: u64::from(line) * 30 + 4,
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

/// The raw index rows of each page statement of `what` since the last call.
fn page_rows(what: &str) -> Vec<usize> {
    store::PAGE_ROWS.with(|p| {
        p.borrow_mut()
            .drain(..)
            .filter(|(w, _)| *w == what)
            .map(|(_, n)| n)
            .collect()
    })
}

/// Units of `n` files each defining and referring to `name` once, OIDs
/// from `first`.
fn naming(first: u32, n: u32, name: &str) -> Vec<(BlobId, Extraction)> {
    (first..first + n)
        .map(|i| (oid(i), done(vec![def(name, DefKind::Function, 1)], &[name])))
        .collect()
}

fn commit_all(store: &Store, version: &Version, units: &[(BlobId, Extraction)]) {
    let rows: Vec<_> = units.iter().map(|(o, e)| (*o, version, e)).collect();
    for chunk in rows.chunks(64) {
        commit(store, chunk);
    }
}

/// The pages of a lookup by name bound the index rows each statement
/// reads, not the rows that pass: 600 units of the name under an old
/// version (never deleted before eviction) before three current ones, and
/// 600 in another language, cost statements of at most a page each, and
/// every current definition and reference still comes back.
#[test]
fn store_lookup_pages_bound_the_rows_read() {
    let f = Fixture::new();
    let store = f.store();
    let old = v("ts", 1, 1);
    let current = v("ts", 2, 1);
    let python = v("python", 1, 1);
    commit_all(&store, &old, &naming(0, 600, "get"));
    commit_all(&store, &current, &naming(1000, 3, "get"));
    commit_all(&store, &python, &naming(2000, 600, "get"));
    page_rows("defs");
    page_rows("refs");

    let defs = store
        .defs_by_name("get", std::slice::from_ref(&current))
        .expect("read");
    let mut oids: Vec<BlobId> = defs.iter().map(|d| d.oid).collect();
    oids.sort();
    assert_eq!(oids, vec![oid(1000), oid(1001), oid(1002)]);
    let rows = page_rows("defs");
    assert!(rows.iter().all(|&r| r <= store::NAME_PAGE), "{rows:?}");
    assert_eq!(rows.iter().sum::<usize>(), 1203, "every row read once");
    assert_eq!(rows.len(), 1203usize.div_ceil(store::NAME_PAGE));

    let refs = store
        .refs_by_name("get", std::slice::from_ref(&current))
        .expect("read");
    assert_eq!(refs.len(), 3);
    let rows = page_rows("refs");
    assert!(rows.iter().all(|&r| r <= store::NAME_PAGE), "{rows:?}");
    assert_eq!(rows.iter().sum::<usize>(), 1203);
    // Cut short after one page: the units not read are counted, any
    // version.
    let mut pager = NamePager::new("get", std::slice::from_ref(&current));
    assert_eq!(store.refs_left(&pager), 1203);
    store.next_refs(&mut pager, store::NAME_PAGE).expect("read");
    assert_eq!(store.refs_left(&pager), 1203 - store::NAME_PAGE as u64);
    page_rows("refs");

    // A language no unit of the name is in: bounded pages, nothing found.
    let none = store
        .defs_by_name("get", std::slice::from_ref(&v("go", 1, 1)))
        .expect("read");
    assert!(none.is_empty());
    let rows = page_rows("defs");
    assert!(rows.iter().all(|&r| r <= store::NAME_PAGE), "{rows:?}");
}

/// The name table's scan in pages: more entries than one statement reads,
/// spellings equal but for case interleaved by unit, only the wanted
/// units' rows: each distinct spelling once with the union of its kinds,
/// as a direct computation gives; `stop` ends it between statements.
#[test]
fn store_def_names_in_pages() {
    use std::collections::BTreeMap;
    let f = Fixture::new();
    let store = f.store();
    let js = v("js", 1, 1);
    let kinds = [DefKind::Function, DefKind::Class, DefKind::Method];
    let spell = ["get", "Get", "GET"];
    let units: Vec<(BlobId, Extraction)> = (0..3000u32)
        .map(|i| {
            let defs = vec![
                def(spell[i as usize % 3], kinds[i as usize % 3], 1),
                def(spell[(i as usize + 1) % 3], kinds[(i as usize / 3) % 3], 2),
                def(&format!("n{}", i % 50), kinds[i as usize % 2], 3),
            ];
            (oid(i), done(defs, &[]))
        })
        .collect();
    commit_all(&store, &js, &units);
    let all = store.units(std::slice::from_ref(&js)).expect("read");
    // Every other unit is wanted.
    let mut wanted: Vec<Unit> = all
        .iter()
        .filter(|u| u.oid.as_bytes()[3] % 2 == 0)
        .map(|u| u.unit)
        .collect();
    wanted.sort();
    let mut expected: BTreeMap<String, u16> = BTreeMap::new();
    for u in &wanted {
        for d in store.load(*u).expect("unit").defs {
            *expected.entry(d.name.to_string()).or_insert(0) |= crate::def_kind_bit(d.kind);
        }
    }
    page_rows("names");
    let mut seen: Vec<(String, u16)> = Vec::new();
    assert!(store.def_names(&wanted, &|| false, |n, k| seen.push((n.to_owned(), k))));
    let rows = page_rows("names");
    assert!(rows.len() >= 3, "{rows:?}");
    assert!(
        rows.iter().all(|&r| r <= store::NAMES_SCAN_PAGE),
        "{rows:?}"
    );
    assert_eq!(rows.iter().sum::<usize>(), 9000, "every entry read once");
    let names: Vec<&String> = seen.iter().map(|(n, _)| n).collect();
    let mut distinct = names.clone();
    distinct.sort();
    distinct.dedup();
    assert_eq!(names.len(), distinct.len(), "each spelling once: {names:?}");
    seen.sort();
    assert_eq!(seen, expected.into_iter().collect::<Vec<_>>());
    // Asked to stop after the first statement.
    let calls = std::cell::Cell::new(0);
    let stop = || {
        calls.set(calls.get() + 1);
        calls.get() > 1
    };
    assert!(!store.def_names(&wanted, &stop, |_, _| {}));
    assert_eq!(page_rows("names").len(), 1);
}

/// One unit with more rows of a name than a page holds (600 overloads):
/// every one comes back, once, across pages; nothing is dropped.
#[test]
fn store_lookup_pages_go_on_inside_a_unit() {
    let f = Fixture::new();
    let store = f.store();
    let js = v("js", 1, 1);
    let overloads: Vec<Def> = (0..600)
        .map(|i| def("get", [DefKind::Function, DefKind::Method][i % 2], i as u32))
        .collect();
    commit(&store, &[(oid(1), &js, &done(overloads, &[]))]);
    page_rows("defs");
    let mut pager = NamePager::new("get", std::slice::from_ref(&js));
    let mut lines = Vec::new();
    while !pager.done() {
        lines.extend(
            store
                .next_defs(&mut pager, 256)
                .expect("read")
                .into_iter()
                .map(|d| d.def.line),
        );
    }
    lines.sort();
    assert_eq!(lines, (0..600).collect::<Vec<u32>>());
    let rows = page_rows("defs");
    assert!(rows.iter().all(|&r| r <= 256), "{rows:?}");
    assert_eq!(rows.iter().sum::<usize>(), 600);
}

/// A3: a name's definitions in pages of at most the limit of index rows,
/// in index order (a unit's overloads may span pages), every definition
/// once; the rows left counted while pages remain; the other cases of the
/// name read but not returned.
#[test]
fn store_lookup_by_name_in_pages() {
    let f = Fixture::new();
    let store = f.store();
    let js = v("js", 1, 1);
    let mut units = Vec::new();
    for i in 0..10u32 {
        let mut defs = vec![def("get", DefKind::Function, 1)];
        if i % 4 == 2 {
            // An overload in the same unit.
            defs.push(def("get", DefKind::Method, 7));
        }
        if i == 5 {
            defs.push(def("Get", DefKind::Class, 9));
        }
        units.push((oid(i), done(defs, &[])));
    }
    let rows: Vec<_> = units.iter().map(|(o, e)| (*o, &js, e)).collect();
    commit(&store, &rows);
    let all = store
        .defs_by_name("get", std::slice::from_ref(&js))
        .expect("read");
    assert_eq!(all.len(), 12);

    let mut pager = NamePager::new("get", std::slice::from_ref(&js));
    let mut seen = Vec::new();
    let mut pages = 0;
    while !pager.done() {
        let left = store.defs_left(&pager);
        let page = store.next_defs(&mut pager, 3).expect("read");
        assert!(page.len() <= 3, "{page:?}");
        assert!(left >= page.len() as u64);
        seen.extend(page.into_iter().map(|d| (d.oid, d.def.line)));
        pages += 1;
        assert!(pages < 20, "the pages end");
    }
    assert_eq!(store.defs_left(&pager), 0);
    let mut want: Vec<_> = all.iter().map(|d| (d.oid, d.def.line)).collect();
    want.sort();
    seen.sort();
    assert_eq!(seen, want, "each definition once");
    // Before reading anything: every row of the name, any case.
    let pager = NamePager::new("get", std::slice::from_ref(&js));
    assert_eq!(store.defs_left(&pager), 13);
}

/// A3: the lookup by name in given units returns the name's definitions
/// in the units of those OIDs only, at most the limit per statement.
#[test]
fn store_lookup_by_name_in_given_units() {
    let f = Fixture::new();
    let store = f.store();
    let ts = v("ts", 1, 1);
    let units: Vec<_> = (0..700u32)
        .map(|i| {
            (
                oid(i),
                done(vec![def("get", DefKind::Function, i % 50)], &[]),
            )
        })
        .collect();
    let rows: Vec<_> = units.iter().map(|(o, e)| (*o, &ts, e)).collect();
    for chunk in rows.chunks(64) {
        commit(&store, chunk);
    }
    let wanted: Vec<BlobId> = (0..700).filter(|i| i % 7 == 0).map(oid).collect();
    let mut found: Vec<BlobId> = store
        .defs_by_name_in("get", std::slice::from_ref(&ts), &wanted, 256)
        .expect("read")
        .into_iter()
        .map(|d| d.oid)
        .collect();
    found.sort();
    let mut expected = wanted.clone();
    expected.sort();
    assert_eq!(found, expected);
    // 600 OIDs: two statements of at most 5 rows each.
    let many: Vec<BlobId> = (0..600).map(oid).collect();
    let limited = store
        .defs_by_name_in("get", std::slice::from_ref(&ts), &many, 5)
        .expect("read");
    assert_eq!(limited.len(), 10);
    assert_eq!(
        store
            .defs_by_name_in("other", std::slice::from_ref(&ts), &wanted, 256)
            .expect("read"),
        vec![]
    );
}

/// The plans of the lookups by name and the name table's pages (`EXPLAIN
/// QUERY PLAN`): every one seeks `ci_def_name` (or `ci_ref`'s key) by name
/// and cursor, never scans a table and never sorts, so its cost follows the
/// rows or units it reads.
#[test]
fn store_lookups_by_name_use_the_name_and_unit_index() {
    use std::sync::LazyLock;
    static PLANS: LazyLock<Tables> = LazyLock::new(|| {
        let mut statements: Vec<&'static str> = STATEMENTS.to_vec();
        for sql in [
            store::DEFS_PAGE,
            store::DEFS_IN,
            store::DEFS_LEFT,
            store::REFS_PAGE,
            store::NAMES_PAGE,
        ] {
            statements.push(Box::leak(
                format!("EXPLAIN QUERY PLAN {sql}").into_boxed_str(),
            ));
        }
        Tables {
            namespace: NAMESPACE,
            schema: SCHEMA,
            statements: Box::leak(statements.into_boxed_slice()),
            quota: store::QUOTA,
        }
    });
    let f = Fixture::new();
    let p = f.partition();
    let store = Store::with_tables(&p, &PLANS);
    let js = v("js", 1, 1);
    commit(
        &store,
        &[(
            oid(1),
            &js,
            &done(vec![def("get", DefKind::Function, 1)], &[]),
        )],
    );
    let set = p.tables(&PLANS);
    let plan = |sql: &str, params: &[Param<'_>]| -> String {
        let explain: &'static str = PLANS
            .statements
            .iter()
            .find(|s| s.starts_with("EXPLAIN") && s.ends_with(sql))
            .expect("registered");
        set.read(|r| {
            let mut details = Vec::new();
            assert!(r.query(explain, params, |row| {
                details.push(row.text(3).unwrap_or_default().to_owned());
                true
            }));
            details.join("\n")
        })
        .expect("read")
    };
    let cursor = [Param::Int(0), Param::Int(0), Param::Int(0)];
    let page = vec![
        Param::Text("get"),
        cursor[0],
        cursor[1],
        cursor[2],
        Param::Int(256),
    ];
    let mut within = vec![Param::Null; 14 + EXISTS_BATCH];
    within[0] = Param::Text("get");
    within[13] = Param::Int(256);
    for (sql, params, wants) in [
        (
            store::DEFS_PAGE,
            page.clone(),
            &[
                "COVERING INDEX ci_def_name (name=? AND (unit,kind)>(?,?))",
                "e USING INTEGER PRIMARY KEY",
                "d USING INTEGER PRIMARY KEY",
            ][..],
        ),
        (
            store::REFS_PAGE,
            vec![Param::Text("get"), Param::Int(0), Param::Int(256)],
            &[
                "ci_ref USING PRIMARY KEY (name=? AND unit>?)",
                "e USING INTEGER PRIMARY KEY",
            ][..],
        ),
        (
            store::DEFS_IN,
            within,
            &["(oid=?", "ci_def_name (name=? AND unit=?)"][..],
        ),
        (
            store::DEFS_LEFT,
            page[..4].to_vec(),
            &["COVERING INDEX ci_def_name (name=? AND (unit,kind)>(?,?))"][..],
        ),
        (
            store::NAMES_PAGE,
            page,
            &[
                "MERGE",
                "COVERING INDEX ci_def_name (name=? AND (unit,kind)>(?,?))",
                "COVERING INDEX ci_def_name (name>?)",
            ][..],
        ),
    ] {
        let plan = plan(sql, &params);
        for want in wants {
            assert!(plan.contains(want), "{want} in:\n{plan}");
        }
        // The page's own rows (`raw`, a co-routine) are the only scan.
        let scans = plan
            .lines()
            .filter(|l| l.starts_with("SCAN") && *l != "SCAN raw");
        assert_eq!(scans.count(), 0, "{plan}");
        assert!(!plan.contains("TEMP B-TREE"), "{plan}");
    }
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
    assert!(store.def_names(&ids, &|| false, |name, kinds| {
        names.push((name.to_string(), kinds))
    }));
    names.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(
        names,
        vec![
            ("New".to_string(), crate::def_kind_bit(DefKind::Type)),
            ("Shared".to_string(), crate::def_kind_bit(DefKind::Function))
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

fn window(start: u64, end: u64, line: u32) -> UnitMeta {
    UnitMeta {
        part: Some(Part {
            start,
            end,
            line,
            column: 0,
        }),
        canonical: true,
        len: None,
    }
}

fn marker(len: u64) -> (Extraction, UnitMeta) {
    (
        Extraction {
            status: ExtractStatus::Windowed,
            defs: Vec::new(),
            refs: Vec::new(),
        },
        UnitMeta {
            part: None,
            canonical: true,
            len: Some(len),
        },
    )
}

#[test]
fn store_windows_are_units_of_their_own() {
    let f = Fixture::new();
    let store = f.store();
    let ts = v("ts", 1, 1);
    let a = done(vec![def("alpha", DefKind::Function, 0)], &["beta"]);
    let b = done(vec![def("beta", DefKind::Function, 900)], &["alpha"]);
    let mut w = store.write();
    // Out of order: the windows come back by start.
    assert!(w.add_unit(oid(1), &ts, &b, window(100, 250, 900)));
    assert!(w.add_unit(oid(1), &ts, &a, window(0, 100, 0)));
    assert_eq!(w.commit(), Written::Committed);
    // The existence check and the hints look at the whole extraction only.
    assert_eq!(store.exists(&ts, &[oid(1)]), Some(vec![]));
    let rows = store.windows(oid(1), &ts).expect("read");
    let parts: Vec<(u64, u64, u32)> = rows
        .iter()
        .map(|r| (r.part.start, r.part.end, r.part.line))
        .collect();
    assert_eq!(parts, vec![(0, 100, 0), (100, 250, 900)]);
    assert!(
        rows.iter()
            .all(|r| r.status == ExtractStatus::Done && r.canonical)
    );
    // Lookups by name read window units like any `Done` unit.
    let found = store
        .defs_by_name("beta", std::slice::from_ref(&ts))
        .expect("read");
    assert_eq!(found.len(), 1);
    assert_eq!(
        (found[0].def.line, found[0].byte()),
        (900, Some(900 * 30 + 4))
    );
    let refs = store
        .refs_by_name("alpha", std::slice::from_ref(&ts))
        .expect("read");
    assert_eq!(refs.len(), 1);
    assert_eq!(
        refs[0].part.map(|p| (p.start, p.end, p.line)),
        Some((100, 250, 900))
    );
    assert_eq!(
        store.units(std::slice::from_ref(&ts)).map(|u| u.len()),
        Some(2)
    );
    let (all, canonical) = store.load_windows(oid(1), &ts).expect("read");
    let names: Vec<&str> = all.defs.iter().map(|d| &*d.name).collect();
    assert_eq!((names, canonical), (vec!["alpha", "beta"], true));
    assert_eq!(all.refs, vec![Box::from("alpha"), Box::from("beta")]);
}

#[test]
fn store_windowed_marker_needs_its_whole_chain() {
    let f = Fixture::new();
    let store = f.store();
    let ts = v("ts", 1, 1);
    let one = done(vec![def("one", DefKind::Function, 0)], &[]);
    let (mark, meta) = marker(300);
    let mut w = store.write();
    assert!(w.add_unit(oid(1), &ts, &one, window(0, 100, 0)));
    assert!(w.add_unit(oid(1), &ts, &one, window(100, 300, 10)));
    assert!(w.add_unit(oid(1), &ts, &mark, meta));
    assert_eq!(w.commit(), Written::Committed);
    let (found, windows) = store.exists_with_windows(&ts, &[oid(1)]).expect("read");
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].status, ExtractStatus::Windowed);
    assert_eq!(windows.len(), 2, "touched with the marker");

    // A window evicted: the marker no longer holds the file's content.
    let mut w = store.write();
    w.remove_window(oid(1), &ts, 100);
    assert_eq!(w.commit(), Written::Committed);
    let (found, windows) = store.exists_with_windows(&ts, &[oid(1)]).expect("read");
    assert_eq!(found[0].status, ExtractStatus::TooLarge);
    assert!(windows.is_empty());

    // A window that timed out leaves the chain incomplete too.
    let timed_out = Extraction::empty(ExtractStatus::TimedOut);
    let mut w = store.write();
    assert!(w.add_unit(oid(1), &ts, &timed_out, window(100, 300, 10)));
    assert_eq!(w.commit(), Written::Committed);
    assert_eq!(
        store.exists(&ts, &[oid(1)]).expect("read")[0].status,
        ExtractStatus::TooLarge
    );
}

/// A `Windowed` hit queued as the batch state queues it (`Batch::
/// queue_touches` with the windows `exists_with_windows` gave) marks its
/// window units used with the marker: over the quota the older units go,
/// the opened large file's windows stay, and the marker still holds the
/// file's content.
#[test]
fn store_marker_touch_keeps_its_windows() {
    let f = Fixture::new();
    let p = f.partition();
    let store = Store::with_tables(&p, &SMALL_QUOTA);
    let rust = v("rust", 1, 1);
    let extractions: Vec<Extraction> = (0..70).map(sized).collect();
    let big = oid(1000);
    // Ten windows of about 1 KiB and their marker, then 20 other units.
    let mut w = store.write();
    for i in 0..10u64 {
        assert!(w.add_unit(
            big,
            &rust,
            &extractions[i as usize],
            window(i * 100, (i + 1) * 100, i as u32 * 10)
        ));
    }
    let (mark, meta) = marker(1000);
    assert!(w.add_unit(big, &rust, &mark, meta));
    for i in 10..30 {
        assert!(w.add(oid(i), &rust, &extractions[i as usize]));
    }
    assert_eq!(w.commit(), Written::Committed);

    // Two hours later only the large file is checked (it is opened again).
    f.clock.advance(Duration::from_secs(2 * 3600));
    let generation = store.evictions();
    let (found, windows) = store.exists_with_windows(&rust, &[big]).expect("read");
    assert_eq!(found[0].status, ExtractStatus::Windowed);
    assert_eq!(windows.len(), 10);
    let mut batch = crate::index::state::Batch::default();
    let _wake = batch.queue_touches(generation, &found, &windows);
    let mut pending = batch.take_touches();
    assert_eq!(pending.len(), 11);
    assert_eq!(store.touch_pending(&mut pending), 0);

    // 40 more units go over the 64 KiB quota.
    let mut w = store.write();
    for i in 30..70 {
        assert!(w.add(oid(i), &rust, &extractions[i as usize]));
    }
    assert_eq!(w.commit(), Written::Committed);
    assert!(store.evictions() > generation, "nothing was evicted");
    let left = found_oids(
        &store
            .exists(&rust, &(10..30).map(oid).collect::<Vec<_>>())
            .expect("read"),
    );
    assert!(left.len() < 20, "the untouched units went first");
    let (found, windows) = store.exists_with_windows(&rust, &[big]).expect("read");
    assert_eq!(found[0].status, ExtractStatus::Windowed);
    assert_eq!(windows.len(), 10, "the windows stayed with their marker");
}

#[test]
fn store_chain_complete_follows_the_ends() {
    let row = |start: u64, end: u64, status: ExtractStatus| WindowRow {
        unit: Unit::from_id(start as i64 + 1),
        part: Part {
            start,
            end,
            line: 0,
            column: 0,
        },
        status,
        canonical: true,
    };
    let done = ExtractStatus::Done;
    assert!(chain_complete(&[row(0, 10, done), row(10, 20, done)], 20));
    assert!(!chain_complete(&[row(0, 10, done), row(10, 20, done)], 30));
    assert!(!chain_complete(&[row(0, 10, done), row(12, 20, done)], 20));
    assert!(!chain_complete(&[row(10, 20, done)], 20));
    assert!(!chain_complete(
        &[row(0, 10, ExtractStatus::TimedOut), row(10, 20, done)],
        20
    ));
    // A stale window off the chain (another cut) is passed over.
    assert!(chain_complete(
        &[row(0, 10, done), row(5, 15, done), row(10, 20, done)],
        20
    ));
    assert!(!chain_complete(&[], 0));
}

/// Version 2's tables, as the last release wrote them.
static VERSION_2: Tables = Tables {
    namespace: lectern_cache::Namespace {
        name: "code_intel.tags",
        version: 2,
        compression: lectern_cache::Compression::None,
    },
    schema: &[
        "CREATE TABLE ci_ext (
           unit    INTEGER PRIMARY KEY REFERENCES entry (id) ON DELETE CASCADE,
           oid     BLOB    NOT NULL,
           lang    TEXT    NOT NULL,
           grammar INTEGER NOT NULL,
           query   INTEGER NOT NULL,
           status  INTEGER NOT NULL,
           UNIQUE (oid, lang, grammar, query))",
        "CREATE TABLE ci_def (
           unit     INTEGER NOT NULL REFERENCES entry (id) ON DELETE CASCADE,
           name     TEXT    NOT NULL,
           kind     INTEGER NOT NULL,
           line     INTEGER NOT NULL,
           col      INTEGER NOT NULL,
           end_line INTEGER NOT NULL,
           scope    TEXT,
           flags    INTEGER NOT NULL,
           arity    INTEGER)",
        "CREATE INDEX ci_def_name ON ci_def (name COLLATE NOCASE, unit, kind)",
        "CREATE INDEX ci_def_unit ON ci_def (unit)",
        "CREATE TABLE ci_ref (
           name TEXT    NOT NULL,
           unit INTEGER NOT NULL REFERENCES entry (id) ON DELETE CASCADE,
           PRIMARY KEY (name, unit)) WITHOUT ROWID",
        "CREATE INDEX ci_ref_unit ON ci_ref (unit)",
    ],
    statements: &[
        "INSERT INTO ci_ext (unit, oid, lang, grammar, query, status)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        "INSERT INTO ci_def (unit, name, kind, line, col, end_line, scope, flags, arity)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL, 0, NULL)",
    ],
    quota: store::QUOTA,
};

#[test]
fn location_byte_version_2_tables_are_rebuilt() {
    let f = Fixture::new();
    let p = f.partition();
    let rust = v("rust", 1, 1);
    let old = p.tables(&VERSION_2);
    let mut w = old.write();
    let unit = w.create(&lectern_cache::Key::new().bytes(oid(1).as_bytes()), 100);
    let unit = Param::New(unit);
    w.execute(
        VERSION_2.statements[0],
        &[
            unit,
            Param::Bytes(oid(1).as_bytes()),
            Param::Text("rust"),
            Param::Int(1),
            Param::Int(1),
            Param::Int(0),
        ],
    );
    w.execute(
        VERSION_2.statements[1],
        &[
            unit,
            Param::Text("old"),
            Param::Int(1),
            Param::Int(0),
            Param::Int(0),
            Param::Int(0),
        ],
    );
    assert_eq!(w.commit(), Written::Committed);
    drop(old);
    // Version 3's first use drops version 2's tables: nothing of the old
    // unit is found, and the new tables take the byte offsets.
    let store = Store::new(&p);
    let before = store.evictions();
    assert_eq!(store.exists(&rust, &[oid(1)]), Some(vec![]));
    assert_eq!(
        store.defs_by_name("old", std::slice::from_ref(&rust)),
        Some(vec![])
    );
    assert!(store.evictions() > before, "the old tables went");
    commit(
        &store,
        &[(
            oid(1),
            &rust,
            &done(vec![def("new", DefKind::Function, 3)], &[]),
        )],
    );
    let found = store
        .defs_by_name("new", std::slice::from_ref(&rust))
        .expect("read");
    assert_eq!(found[0].byte(), Some(3 * 30 + 4));
}
