//! The packages' path tables (「身份与包的路径表」): one `code_intel.deps`
//! unit per (ecosystem, name, version, identity, package directory), its
//! row in `cd_pkg` and one row per file of the closure in `cd_pkg_file`.
//! Written in the same namespace and transactions as the units they list,
//! evicted by the same quota; an evicted table is an identity not seen.

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
    pub files: Vec<WalkFile>,
}

impl PathTable {
    pub(crate) fn same_key(&self, other: &PathTable) -> bool {
        self.origin.package == other.origin.package
            && self.origin.version == other.origin.version
            && self.identity == other.identity
            && self.dir == other.dir
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
pub(crate) fn key(
    eco: Ecosystem,
    name: &str,
    version: Option<&str>,
    identity: &[u8],
    dir: &str,
) -> Key {
    Key::new()
        .str("pkg")
        .u64(eco_code(eco) as u64)
        .str(name)
        .u64(u64::from(version.is_some()))
        .str(version.unwrap_or(""))
        .bytes(identity)
        .str(dir)
}

/// Records `table` (replacing one of the same key) in `w`. No IO.
pub(crate) fn write(w: &mut StoreWrite, table: &PathTable) {
    let o = &table.origin;
    let unit = w.write.create(
        &key(
            o.ecosystem,
            &o.package,
            o.version.as_deref(),
            &table.identity,
            &table.dir,
        ),
        table.bytes(),
    );
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
    origin: &Arc<DepOrigin>,
    identity: &[u8],
    dir: &str,
) -> Option<PathTable> {
    let params = [
        Param::Text(&origin.package),
        Param::Int(eco_code(origin.ecosystem)),
        origin.version.as_deref().map_or(Param::Null, Param::Text),
        Param::Bytes(identity),
        Param::Text(dir),
    ];
    read_one(store, PKG_BY_KEY, &params, origin)
}

/// The newest path table of `origin`'s package name, whatever its version,
/// identity or directory: the stat table when the identity changed.
pub(crate) fn latest(store: &Store, origin: &Arc<DepOrigin>) -> Option<PathTable> {
    let params = [
        Param::Text(&origin.package),
        Param::Int(eco_code(origin.ecosystem)),
    ];
    read_one(store, PKG_LATEST, &params, origin)
}

fn read_one(
    store: &Store,
    sql: &'static str,
    params: &[Param<'_>],
    origin: &Arc<DepOrigin>,
) -> Option<PathTable> {
    store
        .set
        .read(|r| {
            let mut found = None;
            r.query(sql, params, |row| {
                found = table_at(row, origin);
                false
            });
            let mut table = found?;
            let unit = table.unit?;
            r.query(PKG_FILES, &[Param::Int(unit.id())], |row| {
                if let Some(f) = file_at(row) {
                    table.files.push(f);
                }
                true
            });
            Some(table)
        })
        .flatten()
}

/// The columns `unit, version, identity, dev, ino, size, mtime, ctime, dir,
/// recorded, types`.
fn table_at(row: &Row<'_>, origin: &Arc<DepOrigin>) -> Option<PathTable> {
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
