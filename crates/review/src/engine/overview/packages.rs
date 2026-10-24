//! The per-package counts of today's changes (`Overview::packages`,
//! docs/design/review.md「概览」 step 1): the count set's files whose
//! workspace-side modification time is in today's local calendar day,
//! each assigned to the sub-project with the longest `dir` that holds its
//! path.

use std::collections::HashMap;

use lectern_services::project::Package;

use super::*;

const DAY_MS: i64 = 24 * 60 * 60 * 1000;

/// The local calendar day (days since 1970-01-01) of a Unix time in
/// milliseconds, as `usage`'s `window::local_date`: the instant plus the
/// zone's offset at that instant.
fn local_day(at_ms: i64, zone: &dyn Fn(i64) -> i32) -> i64 {
    at_ms
        .saturating_add(i64::from(zone(at_ms)) * 1000)
        .div_euclid(DAY_MS)
}

/// The index of the package holding `path`: the longest `dir` that is
/// the path's directory or an ancestor of it ("" is the root, which holds
/// every path). `dirs` maps a package's `dir` to its index.
fn holder(dirs: &HashMap<&[u8], usize>, path: &[u8]) -> Option<usize> {
    path.iter()
        .enumerate()
        .rev()
        .filter(|(_, b)| **b == b'/')
        .find_map(|(i, _)| dirs.get(&path[..i]).copied())
        .or_else(|| dirs.get(&path[..0]).copied())
}

/// Per package, how many of the count set's files were modified today
/// (`now_ms` is in the zone's day), for the packages with at least one,
/// ordered by `dir`. `mtimes` are the files' modification times in
/// nanoseconds, in the set's order; a file without one (deleted,
/// unreadable) is not counted.
pub(super) fn today(
    packages: &[Package],
    count: &ChangeSet,
    mtimes: &[Option<i64>],
    now_ms: i64,
    zone: &dyn Fn(i64) -> i32,
) -> Arc<[(RelPath, u32)]> {
    let dirs: HashMap<&[u8], usize> = packages
        .iter()
        .enumerate()
        .map(|(i, p)| (p.dir.as_bytes(), i))
        .collect();
    let today = local_day(now_ms, zone);
    let mut counts = vec![0_u32; packages.len()];
    for (change, mtime) in count.files().iter().zip(mtimes) {
        let Some(mtime_ms) = mtime.map(|t| t.div_euclid(1_000_000)) else {
            continue;
        };
        if local_day(mtime_ms, zone) != today {
            continue;
        }
        if let Some(i) = holder(&dirs, change.path.as_bytes()) {
            counts[i] += 1;
        }
    }
    let mut out: Vec<(RelPath, u32)> = packages
        .iter()
        .zip(counts)
        .filter(|(_, n)| *n > 0)
        .map(|(p, n)| (RelPath::clone(&p.dir), n))
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out.into()
}
