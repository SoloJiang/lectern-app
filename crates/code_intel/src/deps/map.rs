//! The dependency map (「依赖层」「依赖映射」): per project, in memory, the
//! files of the ready path tables by OID, so a query maps a dependency
//! unit's OID to the files that have it. Replaced whole (a new `Arc`) when a
//! table is ready; a query takes the `Arc` with its snapshot.
//!
//! Compact: the files sorted by OID, the OIDs in one buffer, each file's
//! path relative to its package directory in one string, a package index
//! of two bytes. About 26 bytes plus the path per file, so the
//! [`MAP_FILES`] at most are about 1 MiB.

use std::sync::Arc;

use lectern_services::code_intel::DepOrigin;
use lectern_services::project::{BlobId, RelPath};

use super::{MAP_FILES, PathTable};
use crate::extract::ExtractStatus;

#[derive(Default, Debug)]
pub(crate) struct DepMap {
    pub generation: u64,
    pub packages: Box<[Arc<DepOrigin>]>,
    /// Each package's directory, relative to the worktree root.
    dirs: Box<[RelPath]>,
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
    /// The files of `tables` (a `Done` unit's file with its OID), at most
    /// [`MAP_FILES`]: the tables are taken in order, the files past the
    /// limit left out (only found on demand).
    pub(crate) fn build(generation: u64, tables: &[Arc<PathTable>]) -> DepMap {
        let done = ExtractStatus::Done.code().unwrap_or(0);
        let mut packages: Vec<Arc<DepOrigin>> = Vec::new();
        let mut dirs: Vec<RelPath> = Vec::new();
        let mut rows: Vec<(BlobId, u16, &str)> = Vec::new();
        'tables: for table in tables {
            if packages.len() >= usize::from(u16::MAX) {
                break;
            }
            let p = packages.len() as u16;
            let prefix = format!("{}/", table.dir);
            let mut used = false;
            for f in &table.files {
                let (Some(oid), true) = (f.oid, f.status == done) else {
                    continue;
                };
                let Some(rel) = f.path.strip_prefix(&prefix) else {
                    continue;
                };
                if rows.len() >= MAP_FILES {
                    if used {
                        packages.push(table.origin.clone());
                        dirs.push(table.dir.clone());
                    }
                    break 'tables;
                }
                rows.push((oid, p, rel));
                used = true;
            }
            if used {
                packages.push(table.origin.clone());
                dirs.push(table.dir.clone());
            }
        }
        rows.sort_by(|a, b| {
            a.0.as_bytes()
                .cmp(b.0.as_bytes())
                .then(a.1.cmp(&b.1))
                .then(a.2.cmp(b.2))
        });
        rows.dedup();
        let width = rows.first().map_or(20, |r| r.0.as_bytes().len());
        rows.retain(|r| r.0.as_bytes().len() == width);
        let mut oids = Vec::with_capacity(rows.len() * width);
        let mut pkgs = Vec::with_capacity(rows.len());
        let mut ends = Vec::with_capacity(rows.len());
        let mut buf = String::with_capacity(rows.iter().map(|r| r.2.len()).sum());
        for (oid, p, rel) in &rows {
            oids.extend_from_slice(oid.as_bytes());
            pkgs.push(*p);
            buf.push_str(rel);
            ends.push(buf.len() as u32);
        }
        DepMap {
            generation,
            packages: packages.into(),
            dirs: dirs.into(),
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
            + self.packages.len() * std::mem::size_of::<Arc<DepOrigin>>()
            + self.dirs.len() * std::mem::size_of::<RelPath>()
    }
}
