//! Test support (`test-support` feature): a clock advanced by hand, fault
//! injection, and the entry types, custom tables and generated values of
//! bench's `cache/*`.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::clock::Clock;
use crate::util::lock;
use crate::{Cache, Compression, Config, Entry, Namespace, Partition};

/// A clock that moves only when the test advances it. Clones share the time.
#[derive(Clone)]
pub struct ManualClock {
    ms: Arc<AtomicU64>,
}

impl ManualClock {
    pub fn new(start: SystemTime) -> ManualClock {
        let ms = start
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as u64);
        ManualClock {
            ms: Arc::new(AtomicU64::new(ms)),
        }
    }

    pub fn advance(&self, by: Duration) {
        self.ms.fetch_add(by.as_millis() as u64, Ordering::Relaxed);
    }

    pub fn now(&self) -> SystemTime {
        crate::clock::to_system(self.now_ms())
    }

    pub(crate) fn now_ms(&self) -> u64 {
        self.ms.load(Ordering::Relaxed)
    }
}

impl Cache {
    /// A cache whose access times, write pauses and `meta.opened` follow
    /// `clock`.
    pub fn with_clock(config: Config, clock: ManualClock) -> Cache {
        Cache::with(config, Clock::manual(clock))
    }

    /// Turns the debug-build panic on `Config::ui_thread` off, to check the
    /// release behavior.
    pub fn set_ui_thread_panics(&self, panics: bool) {
        self.inner
            .shared
            .ui_thread_panics
            .store(panics, Ordering::Relaxed);
    }
}

impl Partition {
    /// The partition's `.sqlite` file (its `-wal` and `-shm` sit next to it).
    pub fn file(&self) -> &Path {
        &self.slot.file
    }

    /// Sets `PRAGMA max_page_count` on the write connection before every
    /// following write transaction, to make `SQLITE_FULL`; `None` lifts the
    /// limit. SQLite never lowers it below the current size.
    pub fn set_max_page_count(&self, pages: Option<u64>) {
        lock(&self.slot.hooks).max_page_count = Some(pages);
    }

    /// Runs `hook` inside every following write transaction, after the
    /// writes and before the commit, with the write lock held.
    pub fn set_write_hook(&self, hook: Option<Arc<dyn Fn() + Send + Sync>>) {
        lock(&self.slot.hooks).in_write = hook;
    }
}

/// Bytes stored as written, in the `test.raw` namespace: for measuring the
/// store itself (bench's `cache/*`, docs/design/bench.md「迁移」).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Raw(pub Vec<u8>);

impl Entry for Raw {
    const NAMESPACE: Namespace = Namespace {
        name: "test.raw",
        version: 1,
        compression: Compression::None,
    };

    fn encode(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.0);
    }

    fn decode(bytes: &[u8]) -> Option<Self> {
        Some(Raw(bytes.to_vec()))
    }
}

/// Bytes in the `test.zstd` namespace, which compresses values of 4 KiB or
/// more (bench's `cache/zstd_1m`).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Zstd(pub Vec<u8>);

impl Entry for Zstd {
    const NAMESPACE: Namespace = Namespace {
        name: "test.zstd",
        version: 1,
        compression: Compression::Zstd,
    };

    fn encode(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.0);
    }

    fn decode(bytes: &[u8]) -> Option<Self> {
        Some(Zstd(bytes.to_vec()))
    }
}

/// `len` bytes from a xorshift generator seeded with `seed`: deterministic
/// and incompressible.
pub fn noise(seed: u64, len: usize) -> Vec<u8> {
    let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x as u8
        })
        .collect()
}

/// `len` bytes of one JSON line repeated, like a session file: compresses
/// well.
pub fn text(len: usize) -> Vec<u8> {
    const LINE: &[u8] = b"{\"type\":\"assistant\",\"text\":\"lorem ipsum\"}\n";
    (0..len).map(|i| LINE[i % LINE.len()]).collect()
}

/// Custom tables shaped like code_intel's (bench's `cache/tables_*`), in
/// the `test.tags` namespace: one row per unit in `tag_ext`, definitions in
/// `tag_def`, reference names in `tag_ref`. Unit `i` has `DEFS` definitions
/// named [`tags::def_name`] and `REFS` references to other units' names.
pub mod tags {
    use crate::{Compression, Key, Namespace, Param, TableWrite, Tables};

    pub const DEFS: u64 = 20;
    pub const REFS: u64 = 40;

    pub const INSERT_EXT: &str = "INSERT INTO tag_ext (unit, oid) VALUES (?1, ?2)";
    pub const INSERT_DEF: &str =
        "INSERT INTO tag_def (unit, name, kind, line, col) VALUES (?1, ?2, ?3, ?4, ?5)";
    pub const INSERT_REF: &str = "INSERT OR IGNORE INTO tag_ref (name, unit) VALUES (?1, ?2)";
    /// Definitions by name, with their unit's OID.
    pub const DEFS_BY_NAME: &str = "SELECT d.line, d.col, e.oid FROM tag_def d
         JOIN tag_ext e ON e.unit = d.unit WHERE d.name = ?1";
    /// Every unit.
    pub const ALL_UNITS: &str = "SELECT unit FROM tag_ext ORDER BY unit";

    const SCHEMA: &[&str] = &[
        "CREATE TABLE tag_ext (
           unit INTEGER PRIMARY KEY REFERENCES entry (id) ON DELETE CASCADE,
           oid  BLOB NOT NULL UNIQUE)",
        "CREATE TABLE tag_def (
           unit INTEGER NOT NULL REFERENCES entry (id) ON DELETE CASCADE,
           name TEXT NOT NULL, kind INTEGER NOT NULL, line INTEGER NOT NULL,
           col INTEGER NOT NULL)",
        "CREATE INDEX tag_def_name ON tag_def (name)",
        "CREATE INDEX tag_def_unit ON tag_def (unit)",
        "CREATE TABLE tag_ref (
           name TEXT NOT NULL,
           unit INTEGER NOT NULL REFERENCES entry (id) ON DELETE CASCADE,
           PRIMARY KEY (name, unit)) WITHOUT ROWID",
        "CREATE INDEX tag_ref_unit ON tag_ref (unit)",
    ];
    const STATEMENTS: &[&str] = &[INSERT_EXT, INSERT_DEF, INSERT_REF, DEFS_BY_NAME, ALL_UNITS];
    const NAMESPACE: Namespace = Namespace {
        name: "test.tags",
        version: 1,
        compression: Compression::None,
    };

    /// The registration, with code_intel's 256 MiB quota.
    pub static TABLES: Tables = Tables {
        namespace: NAMESPACE,
        schema: SCHEMA,
        statements: STATEMENTS,
        quota: 256 << 20,
    };

    /// The same tables with an 8 MiB quota: writes through it into a
    /// partition filled through [`TABLES`] evict to the quota.
    pub static SMALL_QUOTA: Tables = Tables {
        namespace: NAMESPACE,
        schema: SCHEMA,
        statements: STATEMENTS,
        quota: 8 << 20,
    };

    /// The OID of unit `i`.
    pub fn oid(i: u64) -> [u8; 20] {
        let mut oid = [0u8; 20];
        oid[..8].copy_from_slice(&i.to_be_bytes());
        oid
    }

    /// The name of definition `j` of unit `i`; unique.
    pub fn def_name(i: u64, j: u64) -> String {
        format!("symbol_{:08}", i * DEFS + j)
    }

    /// Records unit `i` with its rows, declaring code_intel's estimate of
    /// their size: 64, plus 2 × name + 40 per definition and 2 × name + 24
    /// per reference.
    pub fn add(w: &mut TableWrite, i: u64) {
        let names: Vec<String> = (0..DEFS).map(|j| def_name(i, j)).collect();
        let refs: Vec<String> = (0..REFS)
            .map(|j| def_name((i * 7 + j * 13) % (i + 1), j % DEFS))
            .collect();
        let bytes = 64
            + names.iter().map(|n| 2 * n.len() as u64 + 40).sum::<u64>()
            + refs.iter().map(|n| 2 * n.len() as u64 + 24).sum::<u64>();
        let oid = oid(i);
        let unit = w.create(&Key::new().bytes(&oid).str("rust").u64(1).u64(1), bytes);
        w.execute(INSERT_EXT, &[Param::New(unit), Param::Bytes(&oid)]);
        for (j, name) in names.iter().enumerate() {
            let j = j as i64;
            w.execute(
                INSERT_DEF,
                &[
                    Param::New(unit),
                    Param::Text(name),
                    Param::Int(j % 8),
                    Param::Int(j * 10 + 1),
                    Param::Int(4),
                ],
            );
        }
        for name in &refs {
            w.execute(INSERT_REF, &[Param::Text(name), Param::New(unit)]);
        }
    }
}
