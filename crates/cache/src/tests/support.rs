//! Entry types, fixtures and direct SQLite inspection for the tests.

use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use rusqlite::Connection;
use tempfile::TempDir;

use crate::testing::ManualClock;
use crate::{Budgets, Cache, Compression, Config, Entry, Key, Namespace, Partition};

macro_rules! bytes_entry {
    ($(#[$doc:meta])* $name:ident, $ns:literal, $version:literal, $compression:expr) => {
        $(#[$doc])*
        #[derive(Clone, PartialEq, Eq, Debug)]
        pub(super) struct $name(pub(super) Vec<u8>);

        impl Entry for $name {
            const NAMESPACE: Namespace = Namespace {
                name: $ns,
                version: $version,
                compression: $compression,
            };

            fn encode(&self, out: &mut Vec<u8>) {
                out.extend_from_slice(&self.0);
            }

            fn decode(bytes: &[u8]) -> Option<Self> {
                Some($name(bytes.to_vec()))
            }
        }
    };
}

bytes_entry!(Blob, "test.blob", 1, Compression::None);
bytes_entry!(
    /// The next version of `Blob`'s namespace.
    BlobV2,
    "test.blob",
    2,
    Compression::None
);
bytes_entry!(Other, "test.other", 1, Compression::None);
bytes_entry!(Packed, "test.packed", 1, Compression::Zstd);

/// Rejects values that start with 0xFF.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(super) struct Picky(pub(super) Vec<u8>);

impl Entry for Picky {
    const NAMESPACE: Namespace = Namespace {
        name: "test.picky",
        version: 1,
        compression: Compression::None,
    };

    fn encode(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.0);
    }

    fn decode(bytes: &[u8]) -> Option<Self> {
        (bytes.first() != Some(&0xFF)).then(|| Picky(bytes.to_vec()))
    }
}

/// A temporary cache root and a manual clock.
pub(super) struct Fixture {
    pub(super) dir: TempDir,
    pub(super) clock: ManualClock,
    pub(super) cache: Cache,
}

impl Fixture {
    pub(super) fn new() -> Fixture {
        Fixture::with_budgets(Budgets::default())
    }

    pub(super) fn with_budgets(budgets: Budgets) -> Fixture {
        let dir = tempfile::tempdir().expect("temporary directory");
        let clock = ManualClock::new(UNIX_EPOCH + Duration::from_secs(1_800_000_000));
        let cache = Cache::with_clock(config(&dir.path().join("cache"), budgets), clock.clone());
        Fixture { dir, clock, cache }
    }

    pub(super) fn root(&self) -> PathBuf {
        self.dir.path().join("cache")
    }

    /// Another cache on the same root, as in the next run.
    pub(super) fn reopen(&mut self) {
        self.cache =
            Cache::with_clock(config(&self.root(), Budgets::default()), self.clock.clone());
    }

    pub(super) fn repo(&self) -> Partition {
        self.cache.repo(Path::new("/work/lectern/.git"))
    }
}

pub(super) fn config(root: &Path, budgets: Budgets) -> Config {
    Config {
        root: root.to_path_buf(),
        budgets,
        ui_thread: None,
    }
}

pub(super) fn key(i: u64) -> Key {
    Key::new().str("k").u64(i)
}

/// `len` bytes from a xorshift generator: incompressible.
pub(super) fn noise(seed: u64, len: usize) -> Vec<u8> {
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

/// A connection of the test's own to a partition file, as another process
/// would open it.
pub(super) fn raw(file: &Path) -> Connection {
    Connection::open(file).expect("open the partition file")
}

pub(super) fn count(conn: &Connection, ns: &Namespace) -> i64 {
    conn.query_row(
        "SELECT count(*) FROM entry e JOIN namespace n ON e.ns = n.id
         WHERE n.name = ?1 AND n.version = ?2",
        rusqlite::params![ns.name, ns.version],
        |r| r.get(0),
    )
    .expect("count entries")
}

pub(super) fn meta_bytes(conn: &Connection) -> u64 {
    conn.query_row("SELECT value FROM meta WHERE name = 'bytes'", [], |r| {
        r.get::<_, i64>(0)
    })
    .expect("meta.bytes") as u64
}

pub(super) fn sum_sizes(conn: &Connection) -> u64 {
    conn.query_row("SELECT coalesce(sum(size), 0) FROM entry", [], |r| {
        r.get::<_, i64>(0)
    })
    .expect("sum of sizes") as u64
}

/// The `.sqlite` file and the `-wal` / `-shm` next to it.
pub(super) fn files(file: &Path) -> [PathBuf; 3] {
    let with = |suffix: &str| {
        let mut name = file.as_os_str().to_owned();
        name.push(suffix);
        PathBuf::from(name)
    };
    [file.to_path_buf(), with("-wal"), with("-shm")]
}
