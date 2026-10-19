//! `platform/*` (docs/design/platform.md「性能」): `SafeRoot` against
//! `std::fs` on the same paths, with the production backend chosen by
//! `SafeRoot::open` (`NofollowAny` on macOS, `Openat2` on Linux). The range
//! reads (「范围读取」「性能」) compare `open_file` and `OpenFile::read_ranges`
//! with `std::fs::File` and `FileExt::read_at`.

use std::fs::File;
use std::hint::black_box;
use std::io::Write as _;
use std::ops::Range;
use std::os::unix::fs::FileExt as _;
use std::path::{Path, PathBuf};

use lectern_bench::{Bench, BenchError, Ctx, Measured, Value};
use lectern_platform::{OpenFile, Opened, RelPath, SafeRoot};

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

const KIB: u64 = 1024;

/// The page `file_reader` reads files over 16 MiB by (ADR 0034).
const PAGE: u64 = 64 * KIB;

/// One file of 64 MiB (`smoke` 1 MiB), four levels deep.
struct BigFile {
    root: SafeRoot,
    rel: &'static str,
    abs: PathBuf,
    size: u64,
}

impl BigFile {
    fn new(ctx: &Ctx) -> Result<BigFile, BenchError> {
        let dir = ctx.scratch().join("tree");
        let rel = "d0/d1/d2/big.bin";
        let abs = dir.join(rel);
        std::fs::create_dir_all(abs.parent().expect("has a parent"))?;
        let mib: Vec<u8> = (0..1 << 20).map(|i: u32| (i % 251) as u8).collect();
        let mibs = ctx.scale(64, 1);
        let mut file = File::create(&abs)?;
        for _ in 0..mibs {
            file.write_all(&mib)?;
        }
        file.sync_all()?;
        Ok(BigFile {
            root: SafeRoot::open(&dir)?,
            rel,
            abs,
            size: mibs << 20,
        })
    }

    fn rel(&self) -> RelPath<'_> {
        RelPath::new(self.rel.as_bytes()).expect("a valid relative path")
    }

    fn open(&self) -> Result<OpenFile, BenchError> {
        match self.root.open_file(self.rel())? {
            Opened::File(file) => Ok(file),
            other => Err(BenchError::debug("open_file", other)),
        }
    }
}

/// `File::read_at` of `range` into a new buffer, as `read_ranges` returns
/// one.
fn read_at(file: &File, range: &Range<u64>) -> std::io::Result<Vec<u8>> {
    let mut bytes = vec![0; (range.end - range.start) as usize];
    let n = file.read_at(&mut bytes, range.start)?;
    bytes.truncate(n);
    Ok(bytes)
}

/// The first-screen call (「首屏的一次调用」) for a target in the middle of
/// a 64 MiB file: `open_file`, `read_ranges` of the 8 KiB head and the
/// 132 KiB around the target (two reads, not merged), drop; against
/// `File::open`, `metadata`, two `read_at`, `metadata`.
pub const SAFE_READ_RANGE: Bench = Bench {
    id: "platform/safe_read_range",
    run: safe_read_range,
};

fn safe_read_range(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let f = BigFile::new(ctx)?;
    let t = f.size / 2;
    let ranges = [
        0..(8 * KIB).min(f.size),
        t.saturating_sub(64 * KIB)..(t + 68 * KIB).min(f.size),
    ];
    // Checks the call once, and warms the pages it reads.
    let first = f.open()?.read_ranges(&ranges)?;
    if first.pieces[1].len() as u64 != ranges[1].end - ranges[1].start {
        return Err(BenchError::new("the window was read short"));
    }
    let (subject, baseline) = ctx.compare(
        || {
            // The `OpenFile` is dropped, closing the descriptor, inside the
            // timed call.
            f.open().map(|file| file.read_ranges(black_box(&ranges)))
        },
        || -> std::io::Result<_> {
            let file = File::open(black_box(&f.abs))?;
            let before = file.metadata()?;
            let head = read_at(&file, &ranges[0])?;
            let window = read_at(&file, &ranges[1])?;
            Ok((before, head, window, file.metadata()?))
        },
    );
    Ok(vec![Measured::new(
        "platform/safe_read_range",
        Value::Ratio { subject, baseline },
    )])
}

/// Paging a file over 16 MiB: successive 64 KiB pages on one held
/// `OpenFile`, one `read_ranges` each; against `read_at` and `metadata` on
/// one held `std::fs::File`.
pub const SAFE_READ_PAGE: Bench = Bench {
    id: "platform/safe_read_page",
    run: safe_read_page,
};

fn safe_read_page(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let f = BigFile::new(ctx)?;
    let pages = f.size / PAGE;
    let page = |i: u64| {
        let start = (i % pages) * PAGE;
        start..start + PAGE
    };
    let held = f.open()?;
    let std_file = File::open(&f.abs)?;
    // Warms every page.
    for i in 0..pages {
        held.read_ranges(std::slice::from_ref(&page(i)))?;
    }
    let (mut i, mut j) = (0, 0);
    let (subject, baseline) = ctx.compare(
        || {
            i += 1;
            held.read_ranges(std::slice::from_ref(black_box(&page(i))))
        },
        || -> std::io::Result<_> {
            j += 1;
            let bytes = read_at(&std_file, black_box(&page(j)))?;
            Ok((bytes, std_file.metadata()?))
        },
    );
    Ok(vec![Measured::new(
        "platform/safe_read_page",
        Value::Ratio { subject, baseline },
    )])
}
