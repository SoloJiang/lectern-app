//! The packages' path tables (「身份与包的路径表」): one `code_intel.deps`
//! unit per (worktree, ecosystem, name, version, identity, package
//! directory), its row in `cd_pkg` and one row per file of the closure in
//! `cd_pkg_file`. Written in the same namespace and transactions as the
//! units they list, evicted by the same quota; an evicted table is an
//! identity not seen. The worktree is part of the key: each worktree has
//! its own `node_modules` (other inodes), so worktrees sharing the
//! partition never take or rewrite each other's tables.

use std::sync::Arc;

use lectern_cache::{Key, Param, Row, Unit};
use lectern_services::code_intel::{DepOrigin, Ecosystem};
use lectern_services::project::{BlobId, RelPath};

use super::{Stat, WalkFile};
use crate::store::{
    INSERT_PKG, INSERT_PKG_FILE, PKG_BY_KEY, PKG_FILES, PKG_LATEST, Store, StoreWrite,
};

/// A package's path table.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct PathTable {
    /// Its unit, once stored or read.
    pub unit: Option<Unit>,
    pub origin: Arc<DepOrigin>,
    pub identity: Arc<[u8]>,
    /// The installed `package.json`'s five stats when recorded.
    pub manifest: Stat,
    /// The package directory, relative to the worktree root.
    pub dir: RelPath,
    /// When recorded, wall-clock nanoseconds.
    pub recorded: i64,
    /// The `@types` package that gave this one's declarations.
    pub types: Option<String>,
    /// The worktree's root (the key's first part).
    pub worktree: Arc<str>,
    /// Whether `files` holds the table's rows. A ready table keeps only its
    /// header once the dependency map has its files ([`PathTable::header`]):
    /// the rows are read again from the store when needed.
    pub loaded: bool,
    pub files: Vec<WalkFile>,
}

impl PathTable {
    pub(crate) fn same_key(&self, other: &PathTable) -> bool {
        self.origin.package == other.origin.package
            && self.origin.version == other.origin.version
            && self.identity == other.identity
            && self.dir == other.dir
            && self.worktree == other.worktree
    }

    /// This table without its rows (kept in `Deps::ready`).
    pub(crate) fn header(&self) -> PathTable {
        PathTable {
            unit: self.unit,
            origin: self.origin.clone(),
            identity: self.identity.clone(),
            manifest: self.manifest,
            dir: self.dir.clone(),
            recorded: self.recorded,
            types: self.types.clone(),
            worktree: self.worktree.clone(),
            loaded: false,
            files: Vec::new(),
        }
    }

    /// The bytes its unit declares: about 80 per row plus its path.
    pub(crate) fn bytes(&self) -> u64 {
        let rows: u64 = self.files.iter().map(|f| 80 + f.path.len() as u64).sum();
        128 + self.dir.len() as u64 + self.identity.len() as u64 + rows
    }
}

/// The ecosystem's code in `cd_pkg.eco`.
pub(crate) fn eco_code(eco: Ecosystem) -> i64 {
    match eco {
        Ecosystem::Npm => 0,
    }
}

/// The unit key of a path table.
pub(crate) fn key(table: &PathTable) -> Key {
    let o = &table.origin;
    Key::new()
        .str("pkg")
        .str(&table.worktree)
        .u64(eco_code(o.ecosystem) as u64)
        .str(&o.package)
        .u64(u64::from(o.version.is_some()))
        .str(o.version.as_deref().unwrap_or(""))
        .bytes(&table.identity)
        .str(&table.dir)
}

/// Records `table` (replacing one of the same key) in `w`. No IO.
pub(crate) fn write(w: &mut StoreWrite, table: &PathTable) {
    debug_assert!(table.loaded, "a header has no rows to write");
    let o = &table.origin;
    let unit = w.write.create(&key(table), table.bytes());
    let unit = Param::New(unit);
    let m = &table.manifest;
    w.write.execute(
        INSERT_PKG,
        &[
            unit,
            Param::Int(eco_code(o.ecosystem)),
            Param::Text(&o.package),
            o.version.as_deref().map_or(Param::Null, Param::Text),
            Param::Bytes(&table.identity),
            Param::Int(m.dev as i64),
            Param::Int(m.ino as i64),
            Param::Int(m.size as i64),
            Param::Int(m.mtime),
            Param::Int(m.ctime),
            Param::Text(&table.dir),
            Param::Int(table.recorded),
            table.types.as_deref().map_or(Param::Null, Param::Text),
            Param::Text(&table.worktree),
        ],
    );
    for (seq, f) in table.files.iter().enumerate() {
        w.write.execute(
            INSERT_PKG_FILE,
            &[
                unit,
                Param::Int(seq as i64),
                Param::Text(&f.path),
                f.oid
                    .as_ref()
                    .map_or(Param::Null, |o| Param::Bytes(o.as_bytes())),
                Param::Int(f.stat.dev as i64),
                Param::Int(f.stat.ino as i64),
                Param::Int(f.stat.size as i64),
                Param::Int(f.stat.mtime),
                Param::Int(f.stat.ctime),
                Param::Int(i64::from(f.status)),
            ],
        );
    }
    w.units += 1;
}

/// The path table of exactly this key, with its files; None when not
/// stored (or unreadable). `origin` is the package as the caller found it.
pub(crate) fn by_key(
    store: &Store,
    worktree: &Arc<str>,
    origin: &Arc<DepOrigin>,
    identity: &[u8],
    dir: &str,
) -> Option<PathTable> {
    let params = key_params(worktree, origin, identity, dir);
    read_one(store, PKG_BY_KEY, &params, worktree, origin, true)
}

/// Whether a path table of `table`'s key is stored (its rows not read):
/// writing `table` then replaces a unit, which the cache counts as an
/// eviction of the namespace.
pub(crate) fn stored(store: &Store, table: &PathTable) -> bool {
    let params = key_params(&table.worktree, &table.origin, &table.identity, &table.dir);
    read_one(
        store,
        PKG_BY_KEY,
        &params,
        &table.worktree,
        &table.origin,
        false,
    )
    .is_some()
}

/// The rows of a ready table's header read again: None when its key is no
/// longer stored.
pub(crate) fn reload(store: &Store, header: &PathTable) -> Option<PathTable> {
    by_key(
        store,
        &header.worktree,
        &header.origin,
        &header.identity,
        &header.dir,
    )
}

fn key_params<'a>(
    worktree: &'a str,
    origin: &'a DepOrigin,
    identity: &'a [u8],
    dir: &'a str,
) -> [Param<'a>; 6] {
    [
        Param::Text(&origin.package),
        Param::Int(eco_code(origin.ecosystem)),
        origin.version.as_deref().map_or(Param::Null, Param::Text),
        Param::Bytes(identity),
        Param::Text(dir),
        Param::Text(worktree),
    ]
}

/// The newest path table of `origin`'s package name in `worktree`, whatever
/// its version, identity or directory: the stat table when the identity
/// changed.
pub(crate) fn latest(
    store: &Store,
    worktree: &Arc<str>,
    origin: &Arc<DepOrigin>,
) -> Option<PathTable> {
    let params = [
        Param::Text(&origin.package),
        Param::Int(eco_code(origin.ecosystem)),
        Param::Text(worktree),
    ];
    read_one(store, PKG_LATEST, &params, worktree, origin, true)
}

fn read_one(
    store: &Store,
    sql: &'static str,
    params: &[Param<'_>],
    worktree: &Arc<str>,
    origin: &Arc<DepOrigin>,
    files: bool,
) -> Option<PathTable> {
    store
        .set
        .read(|r| {
            let mut found = None;
            r.query(sql, params, |row| {
                found = table_at(row, worktree, origin);
                false
            });
            let mut table = found?;
            let unit = table.unit?;
            if files {
                r.query(PKG_FILES, &[Param::Int(unit.id())], |row| {
                    if let Some(f) = file_at(row) {
                        table.files.push(f);
                    }
                    true
                });
            }
            table.loaded = files;
            Some(table)
        })
        .flatten()
}

/// The columns `unit, version, identity, dev, ino, size, mtime, ctime, dir,
/// recorded, types`.
fn table_at(row: &Row<'_>, worktree: &Arc<str>, origin: &Arc<DepOrigin>) -> Option<PathTable> {
    let int = |c: usize| row.int(c);
    let version = row.text(1).map(Arc::from);
    let origin = if version == origin.version {
        origin.clone()
    } else {
        Arc::new(DepOrigin {
            version,
            ..(**origin).clone()
        })
    };
    Some(PathTable {
        unit: row.unit(0),
        origin,
        identity: Arc::from(row.bytes(2)?),
        manifest: Stat {
            dev: int(3)? as u64,
            ino: int(4)? as u64,
            size: int(5)? as u64,
            mtime: int(6)?,
            ctime: int(7)?,
        },
        dir: Arc::from(row.text(8)?),
        recorded: int(9)?,
        types: row.text(10).map(str::to_owned),
        worktree: worktree.clone(),
        loaded: false,
        files: Vec::new(),
    })
}

/// The columns `seq, path, oid, dev, ino, size, mtime, ctime, status`.
fn file_at(row: &Row<'_>) -> Option<WalkFile> {
    let int = |c: usize| row.int(c);
    Some(WalkFile {
        path: Arc::from(row.text(1)?),
        oid: row.bytes(2).and_then(BlobId::from_bytes),
        stat: Stat {
            dev: int(3)? as u64,
            ino: int(4)? as u64,
            size: int(5)? as u64,
            mtime: int(6)?,
            ctime: int(7)?,
        },
        status: u8::try_from(int(8)?).ok()?,
    })
}
