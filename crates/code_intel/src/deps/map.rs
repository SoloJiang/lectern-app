//! The dependency map (「依赖层」「依赖映射」): per project, in memory, the
//! files of the ready path tables by OID, so a query maps a dependency
//! unit's OID to the files that have it. Replaced whole (a new `Arc`) when a
//! table is ready; a query takes the `Arc` with its snapshot.
//!
//! Compact: the files sorted by OID, the OIDs in one buffer, each file's
//! path relative to its package directory in one string, a package index
//! and a place in its table of two bytes each. The OID's width plus 8
//! bytes plus the path per file; bounded by [`MAP_FILES`] and by
//! [`MAP_BYTES`], so the map's share holds whatever the paths' lengths and
//! the hash. The tables are taken in `Deps::tables`' order (the current
//! sub-project's direct dependencies in its manifest's order, each one's
//! `@types` package after it) and each table's files breadth-first from its
//! entry, so the bounds cut the last ones.
//!
//! The ready path tables keep only their headers once the map has their
//! files (`deps::install_map`): a rebuild takes a loaded table's files from
//! the table, a header's from the map it replaces. A header whose files the
//! bounds cut stays out of the map until its rows are read again (the next
//! identity task of its sub-project).

use std::collections::HashMap;
use std::sync::Arc;

use lectern_services::code_intel::DepOrigin;
use lectern_services::project::{BlobId, RelPath};

use super::{MAP_BYTES, MAP_FILES, PathTable};
use crate::extract::ExtractStatus;

/// The bytes of one package's entries in the map.
const PACKAGE_BYTES: usize = std::mem::size_of::<Arc<DepOrigin>>()
    + std::mem::size_of::<RelPath>()
    + std::mem::size_of::<Arc<[u8]>>();
/// The bytes of one file besides its OID and path: its package, its place
/// and its path's end.
const FILE_BYTES: usize = 2 + 2 + 4;

#[derive(Default, Debug)]
pub(crate) struct DepMap {
    pub generation: u64,
    pub packages: Box<[Arc<DepOrigin>]>,
    /// Each package's directory, relative to the worktree root.
    dirs: Box<[RelPath]>,
    /// Each package's path table identity: which table a header is.
    idents: Box<[Arc<[u8]>]>,
    /// The files' OIDs in order, `width` bytes each.
    oids: Box<[u8]>,
    width: usize,
    /// Each file's package (an index into `packages`).
    pkgs: Box<[u16]>,
    /// Each file's place in its package's table (breadth-first from the
    /// entry): a rebuild from a header takes the files in this order.
    seqs: Box<[u16]>,
    /// Each file's path relative to its package directory ends here in
    /// `buf`.
    ends: Box<[u32]>,
    buf: Box<str>,
}

/// One file of the map: its package and its path relative to the worktree
/// root.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct DepEntry {
    pub origin: Arc<DepOrigin>,
    pub path: String,
}

impl DepMap {
    /// The files of `tables` (a `Done` unit's file with its OID): a loaded
    /// table's from its rows, a header's from `old` (the map this one
    /// replaces). The tables are taken in order; the files past
    /// [`MAP_FILES`] or [`MAP_BYTES`] are left out (only found on demand).
    pub(crate) fn build(
        generation: u64,
        tables: &[Arc<PathTable>],
        old: Option<&DepMap>,
    ) -> DepMap {
        let done = ExtractStatus::Done.code().unwrap_or(0);
        let mut packages: Vec<Arc<DepOrigin>> = Vec::new();
        let mut dirs: Vec<RelPath> = Vec::new();
        let mut idents: Vec<Arc<[u8]>> = Vec::new();
        // (OID, package, path in the package, place in the table).
        let mut rows: Vec<(&[u8], u16, &str, u16)> = Vec::new();
        let mut bytes = 0usize;
        // The old map's files by package, in one pass: linear in its files.
        let old = old.map(Buckets::of);
        'tables: for table in tables {
            if packages.len() >= usize::from(u16::MAX) {
                break;
            }
            let p = packages.len() as u16;
            // In the table's order (breadth-first from the entry), so a
            // package cut by the bounds keeps the files nearest its entry.
            let files: Vec<(&[u8], &str)> = if table.loaded {
                let prefix = format!("{}/", table.dir);
                table
                    .files
                    .iter()
                    .filter(|f| f.status == done)
                    .filter_map(|f| {
                        let oid = f.oid.as_ref()?.as_bytes();
                        Some((oid, f.path.strip_prefix(prefix.as_str())?))
                    })
                    .collect()
            } else {
                old.as_ref().map(|b| b.files_of(table)).unwrap_or_default()
            };
            let mut used = false;
            for (seq, (oid, rel)) in files.into_iter().enumerate() {
                let cost =
                    oid.len() + FILE_BYTES + rel.len() + if used { 0 } else { PACKAGE_BYTES };
                if rows.len() >= MAP_FILES || bytes + cost > MAP_BYTES {
                    if used {
                        packages.push(table.origin.clone());
                        dirs.push(table.dir.clone());
                        idents.push(table.identity.clone());
                    }
                    break 'tables;
                }
                bytes += cost;
                rows.push((oid, p, rel, u16::try_from(seq).unwrap_or(u16::MAX)));
                used = true;
            }
            if used {
                packages.push(table.origin.clone());
                dirs.push(table.dir.clone());
                idents.push(table.identity.clone());
            }
        }
        rows.sort();
        rows.dedup_by(|a, b| (a.0, a.1, a.2) == (b.0, b.1, b.2));
        let width = rows.first().map_or(20, |r| r.0.len());
        rows.retain(|r| r.0.len() == width);
        let mut oids = Vec::with_capacity(rows.len() * width);
        let mut pkgs = Vec::with_capacity(rows.len());
        let mut seqs = Vec::with_capacity(rows.len());
        let mut ends = Vec::with_capacity(rows.len());
        let mut buf = String::with_capacity(rows.iter().map(|r| r.2.len()).sum());
        for (oid, p, rel, seq) in &rows {
            oids.extend_from_slice(oid);
            pkgs.push(*p);
            seqs.push(*seq);
            buf.push_str(rel);
            ends.push(buf.len() as u32);
        }
        DepMap {
            generation,
            packages: packages.into(),
            dirs: dirs.into(),
            idents: idents.into(),
            oids: oids.into(),
            width,
            pkgs: pkgs.into(),
            seqs: seqs.into(),
            ends: ends.into(),
            buf: buf.into(),
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.pkgs.len()
    }

    fn oid(&self, i: usize) -> &[u8] {
        &self.oids[i * self.width..(i + 1) * self.width]
    }

    fn rel(&self, i: usize) -> &str {
        let start = if i == 0 { 0 } else { self.ends[i - 1] as usize };
        &self.buf[start..self.ends[i] as usize]
    }

    /// The files whose content is `oid`.
    pub(crate) fn paths(&self, oid: &BlobId) -> Vec<DepEntry> {
        let key = oid.as_bytes();
        if key.len() != self.width {
            return Vec::new();
        }
        let n = self.len();
        // The first index whose OID is not below `key`.
        let (mut lo, mut hi) = (0, n);
        while lo < hi {
            let mid = (lo + hi) / 2;
            if self.oid(mid) < key {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        let mut out = Vec::new();
        let mut i = lo;
        while i < n && self.oid(i) == key {
            let p = usize::from(self.pkgs[i]);
            out.push(DepEntry {
                origin: self.packages[p].clone(),
                path: format!("{}/{}", self.dirs[p], self.rel(i)),
            });
            i += 1;
        }
        out
    }

    /// Every file as (package, path relative to the worktree root, OID),
    /// sorted by path (tests).
    #[cfg(test)]
    pub(crate) fn files(&self) -> Vec<(String, String, BlobId)> {
        let mut out: Vec<(String, String, BlobId)> = (0..self.len())
            .filter_map(|i| {
                let p = usize::from(self.pkgs[i]);
                let oid = BlobId::from_bytes(self.oid(i))?;
                Some((
                    self.packages[p].package.to_string(),
                    format!("{}/{}", self.dirs[p], self.rel(i)),
                    oid,
                ))
            })
            .collect();
        out.sort_by(|a, b| a.1.cmp(&b.1));
        out
    }

    /// The heap bytes of the map's own buffers (the packages' `DepOrigin`s
    /// are shared with the path tables).
    #[cfg(test)]
    pub(crate) fn heap_bytes(&self) -> usize {
        self.oids.len()
            + self.pkgs.len() * 2
            + self.ends.len() * 4
            + self.buf.len()
            + self.seqs.len() * 2
            + self.packages.len() * PACKAGE_BYTES
    }
}

/// A package's key in the map: name, version, directory, identity.
type PackageKey<'m> = (&'m str, Option<&'m str>, &'m str, &'m [u8]);

/// An old map's files grouped by package, built in one pass, so a rebuild
/// is linear in the old map's files and the tables, not their product.
struct Buckets<'m> {
    map: &'m DepMap,
    /// A package's key → its index in `map.packages`.
    keys: HashMap<PackageKey<'m>, usize>,
    /// Each package's files (indexes into the map), in table order.
    files: Vec<Vec<u32>>,
}

impl<'m> Buckets<'m> {
    fn of(map: &'m DepMap) -> Buckets<'m> {
        let keys = (0..map.packages.len())
            .map(|p| {
                let o = &map.packages[p];
                let key = (
                    &*o.package,
                    o.version.as_deref(),
                    &*map.dirs[p],
                    &*map.idents[p],
                );
                (key, p)
            })
            .collect();
        let mut files: Vec<Vec<u32>> = vec![Vec::new(); map.packages.len()];
        for i in 0..map.len() {
            files[usize::from(map.pkgs[i])].push(i as u32);
        }
        for f in &mut files {
            f.sort_by_key(|&i| map.seqs[i as usize]);
        }
        Buckets { map, keys, files }
    }

    /// The files the map has of `table`'s package (the same package,
    /// version, directory and identity): (OID, path relative to the
    /// package directory), in table order.
    fn files_of(&self, table: &PathTable) -> Vec<(&'m [u8], &'m str)> {
        let key = (
            &*table.origin.package,
            table.origin.version.as_deref(),
            &*table.dir,
            &*table.identity,
        );
        let Some(&p) = self.keys.get(&key) else {
            return Vec::new();
        };
        self.files[p]
            .iter()
            .map(|&i| (self.map.oid(i as usize), self.map.rel(i as usize)))
            .collect()
    }
}
