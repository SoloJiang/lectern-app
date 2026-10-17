//! `platform/*` (docs/design/platform.md「性能」): `SafeRoot` against
//! `std::fs` on the same paths, with the production backend chosen by
//! `SafeRoot::open` (`NofollowAny` on macOS, `Openat2` on Linux).

use std::hint::black_box;
use std::path::{Path, PathBuf};

use lectern_bench::{Bench, BenchError, Ctx, Measured, Value};
use lectern_platform::{RelPath, SafeRoot};

/// 728 files of 4 KiB, four levels deep, like the repository the budget
/// was set on (`smoke` fewer). Returns the relative paths.
fn tree(root: &Path, files: usize) -> Result<Vec<String>, BenchError> {
    let mut out = Vec::new();
    'outer: for a in 0..4 {
        for b in 0..6 {
            for c in 0..6 {
                for d in 0..6 {
                    if out.len() == files {
                        break 'outer;
                    }
                    let rel = format!("d{a}/d{b}/d{c}/f{d}.rs");
                    let path = root.join(&rel);
                    std::fs::create_dir_all(path.parent().expect("has a parent"))?;
                    std::fs::write(&path, [b'x'; 4096])?;
                    out.push(rel);
                }
            }
        }
    }
    Ok(out)
}

struct Fixture {
    root: SafeRoot,
    rels: Vec<String>,
    abs: Vec<PathBuf>,
}

impl Fixture {
    fn new(ctx: &Ctx) -> Result<Fixture, BenchError> {
        let dir = ctx.scratch().join("tree");
        let rels = tree(&dir, ctx.scale(728, 64))?;
        let abs = rels.iter().map(|r| dir.join(r)).collect();
        Ok(Fixture {
            root: SafeRoot::open(&dir)?,
            rels,
            abs,
        })
    }

    fn rel(&self, i: usize) -> RelPath<'_> {
        RelPath::new(self.rels[i % self.rels.len()].as_bytes()).expect("a valid relative path")
    }

    fn abs(&self, i: usize) -> &Path {
        &self.abs[i % self.abs.len()]
    }
}

/// `SafeRoot::stat` against `std::fs::symlink_metadata`.
pub const SAFE_STAT: Bench = Bench {
    id: "platform/safe_stat",
    run: safe_stat,
};

fn safe_stat(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let f = Fixture::new(ctx)?;
    for i in 0..f.rels.len() {
        f.root.stat(f.rel(i))?;
    }
    let (mut i, mut j) = (0, 0);
    let (subject, baseline) = ctx.compare(
        || {
            i += 1;
            f.root.stat(black_box(f.rel(i)))
        },
        || {
            j += 1;
            std::fs::symlink_metadata(black_box(f.abs(j)))
        },
    );
    Ok(vec![Measured::new(
        "platform/safe_stat",
        Value::Ratio { subject, baseline },
    )])
}

/// `SafeRoot::read` of a 4 KiB file against `std::fs::read`.
pub const SAFE_READ_4K: Bench = Bench {
    id: "platform/safe_read_4k",
    run: safe_read_4k,
};

fn safe_read_4k(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let f = Fixture::new(ctx)?;
    for i in 0..f.rels.len() {
        f.root.read(f.rel(i), 64 << 20)?;
    }
    let (mut i, mut j) = (0, 0);
    let (subject, baseline) = ctx.compare(
        || {
            i += 1;
            f.root.read(black_box(f.rel(i)), 64 << 20)
        },
        || {
            j += 1;
            std::fs::read(black_box(f.abs(j)))
        },
    );
    Ok(vec![Measured::new(
        "platform/safe_read_4k",
        Value::Ratio { subject, baseline },
    )])
}
