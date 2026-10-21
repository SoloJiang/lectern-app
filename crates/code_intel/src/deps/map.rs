//! The dependency map (「依赖层」「依赖映射」): per project, in memory, the
//! files of the ready path tables by OID, so a query maps a dependency
//! unit's OID to the files that have it. Replaced whole (a new `Arc`) when a
//! table is ready; a query takes the `Arc` with its snapshot.
//!
//! Compact: the files sorted by OID, the OIDs in one buffer, each file's
//! path relative to its package directory in one string, a package index
//! of two bytes. The OID's width plus 6 bytes plus the path per file;
//! bounded by [`MAP_FILES`] and by [`MAP_BYTES`], so the map's share holds
//! whatever the paths' lengths and the hash.
//!
//! The ready path tables keep only their headers once the map has their
//! files (`Deps::install_map`): a rebuild takes a loaded table's files from
//! the table, a header's from the map it replaces.

use std::sync::Arc;

use lectern_services::code_intel::DepOrigin;
use lectern_services::project::{BlobId, RelPath};

use super::{MAP_BYTES, MAP_FILES, PathTable};
use crate::extract::ExtractStatus;

/// The bytes of one package's entries in the map.
const PACKAGE_BYTES: usize = std::mem::size_of::<Arc<DepOrigin>>()
    + std::mem::size_of::<RelPath>()
    + std::mem::size_of::<Arc<[u8]>>();

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
        let mut rows: Vec<(&[u8], u16, &str)> = Vec::new();
        let mut bytes = 0usize;
        'tables: for table in tables {
            if packages.len() >= usize::from(u16::MAX) {
                break;
            }
            let p = packages.len() as u16;
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
                old.map(|m| m.files_of(table)).unwrap_or_default()
            };
            let mut used = false;
            for (oid, rel) in files {
                let cost = oid.len() + 6 + rel.len() + if used { 0 } else { PACKAGE_BYTES };
                if rows.len() >= MAP_FILES || bytes + cost > MAP_BYTES {
                    if used {
                        packages.push(table.origin.clone());
                        dirs.push(table.dir.clone());
                        idents.push(table.identity.clone());
                    }
                    break 'tables;
                }
                bytes += cost;
                rows.push((oid, p, rel));
                used = true;
            }
            if used {
                packages.push(table.origin.clone());
                dirs.push(table.dir.clone());
                idents.push(table.identity.clone());
            }
        }
        rows.sort();
        rows.dedup();
        let width = rows.first().map_or(20, |r| r.0.len());
        rows.retain(|r| r.0.len() == width);
        let mut oids = Vec::with_capacity(rows.len() * width);
        let mut pkgs = Vec::with_capacity(rows.len());
        let mut ends = Vec::with_capacity(rows.len());
        let mut buf = String::with_capacity(rows.iter().map(|r| r.2.len()).sum());
        for (oid, p, rel) in &rows {
            oids.extend_from_slice(oid);
            pkgs.push(*p);
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
            ends: ends.into(),
            buf: buf.into(),
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.pkgs.len()
    }

    /// The files this map has of `table`'s package (the same package,
    /// version, identity and directory): (OID, path relative to the package
    /// directory).
    fn files_of(&self, table: &PathTable) -> Vec<(&[u8], &str)> {
        let Some(p) = (0..self.packages.len()).find(|&p| {
            let o = &self.packages[p];
            o.package == table.origin.package
                && o.version == table.origin.version
                && self.dirs[p] == table.dir
                && self.idents[p] == table.identity
        }) else {
            return Vec::new();
        };
        (0..self.len())
            .filter(|&i| usize::from(self.pkgs[i]) == p)
            .map(|i| (self.oid(i), self.rel(i)))
            .collect()
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
            + self.packages.len() * PACKAGE_BYTES
    }
}
