//! Measurements for docs/research/large-files-rendering.md. Single-threaded
//! unless a command says otherwise; every time is wall-clock, best of the
//! repetitions given. See the research doc's 「实测说明」 for how to run it.

mod fixtures;

use std::hint::black_box;
use std::ops::{ControlFlow, Range};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use streaming_iterator::StreamingIterator;
use tree_sitter::{Language, ParseOptions, Parser, Point, Query, QueryCursor, Tree};

// ---------------------------------------------------------------- languages

struct Lang {
    name: &'static str,
    language: Language,
    highlights: String,
}

fn lang_for(path: &Path) -> Lang {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    match ext {
        "ts" => Lang {
            name: "typescript",
            language: tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            // The TS query extends the JS one, as nvim-treesitter and helix do.
            highlights: format!(
                "{}\n{}",
                tree_sitter_javascript::HIGHLIGHT_QUERY,
                tree_sitter_typescript::HIGHLIGHTS_QUERY
            ),
        },
        "js" | "mjs" | "cjs" => Lang {
            name: "javascript",
            language: tree_sitter_javascript::LANGUAGE.into(),
            highlights: tree_sitter_javascript::HIGHLIGHT_QUERY.to_string(),
        },
        "rs" => Lang {
            name: "rust",
            language: tree_sitter_rust::LANGUAGE.into(),
            highlights: tree_sitter_rust::HIGHLIGHTS_QUERY.to_string(),
        },
        "json" => Lang {
            name: "json",
            language: tree_sitter_json::LANGUAGE.into(),
            highlights: tree_sitter_json::HIGHLIGHTS_QUERY.to_string(),
        },
        other => panic!("unsupported extension {other:?}"),
    }
}

fn query_for(lang: &Lang) -> Query {
    match Query::new(&lang.language, &lang.highlights) {
        Ok(q) => q,
        Err(e) => panic!("{}: highlight query: {e}", lang.name),
    }
}

fn parser_for(lang: &Lang) -> Parser {
    let mut p = Parser::new();
    p.set_language(&lang.language).unwrap();
    p
}

// ---------------------------------------------------------------- helpers

fn mib(bytes: usize) -> f64 {
    bytes as f64 / (1024.0 * 1024.0)
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1e3
}

fn allocated() -> usize {
    // SAFETY: mallinfo2 only reads glibc's allocator statistics.
    unsafe { libc::mallinfo2().uordblks }
}

fn trim() {
    // SAFETY: malloc_trim only returns free memory to the system.
    unsafe {
        libc::malloc_trim(0);
    }
}

fn reps_for(len: usize) -> usize {
    if len > 32 << 20 {
        1
    } else if len > 8 << 20 {
        2
    } else {
        3
    }
}

fn best<T>(reps: usize, mut f: impl FnMut() -> T) -> (Duration, T) {
    let mut best = Duration::MAX;
    let mut out = None;
    for _ in 0..reps {
        let start = Instant::now();
        let v = f();
        let d = start.elapsed();
        if d < best {
            best = d;
        }
        out = Some(v);
    }
    (best, out.unwrap())
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// Hash of every node's (kind, start, end): equal trees give equal hashes.
fn tree_hash(tree: &Tree) -> (u64, usize) {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut n = 0usize;
    let mut c = tree.walk();
    loop {
        let node = c.node();
        for v in [
            node.kind_id() as u64,
            node.start_byte() as u64,
            node.end_byte() as u64,
        ] {
            h ^= v;
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
        n += 1;
        if c.goto_first_child() {
            continue;
        }
        loop {
            if c.goto_next_sibling() {
                break;
            }
            if !c.goto_parent() {
                return (h, n);
            }
        }
    }
}

fn line_starts(text: &str) -> Vec<usize> {
    let mut v = Vec::with_capacity(text.len() / 32);
    v.push(0);
    for i in memchr::memchr_iter(b'\n', text.as_bytes()) {
        v.push(i + 1);
    }
    v
}

fn parse_contiguous(p: &mut Parser, text: &str) -> Tree {
    p.reset();
    p.parse(text, None).unwrap()
}

fn parse_chunks(p: &mut Parser, text: &str, chunk: usize) -> Tree {
    p.reset();
    let bytes = text.as_bytes();
    p.parse_with_options(
        &mut |i, _: Point| {
            let start = i.min(bytes.len());
            // Chunks are fixed blocks: the callback returns the rest of the block.
            let end = ((start / chunk) + 1) * chunk;
            &bytes[start..end.min(bytes.len())]
        },
        None,
        None,
    )
    .unwrap()
}

fn parse_rope(p: &mut Parser, rope: &ropey::Rope) -> Tree {
    p.reset();
    p.parse_with_options(
        &mut |i, _: Point| {
            if i >= rope.len() {
                return &[][..];
            }
            let (chunk, start) = rope.chunk(i);
            &chunk.as_bytes()[i - start..]
        },
        None,
        None,
    )
    .unwrap()
}

// ---------------------------------------------------------------- gen

fn cmd_gen(out: &Path) {
    std::fs::create_dir_all(out).unwrap();
    for &size in &[1usize, 4, 16, 64] {
        let bytes = size << 20;
        for (kind, ext, seed) in [
            (fixtures::Kind::Ts, "ts", 1),
            (fixtures::Kind::Rust, "rs", 2),
            (fixtures::Kind::Json, "json", 3),
        ] {
            let text = fixtures::generate(kind, bytes, seed + size as u64);
            let path = out.join(format!("synthetic_{size}m.{ext}"));
            std::fs::write(&path, &text).unwrap();
            println!("{} {} bytes", path.display(), text.len());
        }
    }
    for &size in &[1usize, 4] {
        let text = fixtures::generate(fixtures::Kind::MinJs, size << 20, 7 + size as u64);
        let path = out.join(format!("minified_{size}m.js"));
        std::fs::write(&path, &text).unwrap();
        println!("{} {} bytes", path.display(), text.len());
    }
}

/// Large one-line files for `first`: minified JS and the package-lock shape
/// with all whitespace removed (its strings contain no spaces), 16 and 64 MiB.
fn cmd_gen_oneline(out: &Path) {
    std::fs::create_dir_all(out).unwrap();
    for &size in &[16usize, 64] {
        let text = fixtures::generate(fixtures::Kind::MinJs, size << 20, 7 + size as u64);
        let path = out.join(format!("minified_{size}m.js"));
        std::fs::write(&path, &text).unwrap();
        println!("{} {} bytes", path.display(), text.len());
        let text = fixtures::generate(fixtures::Kind::Json, size << 20, 30 + size as u64)
            .replace(['\n', ' '], "");
        let path = out.join(format!("oneline_{size}m.json"));
        std::fs::write(&path, &text).unwrap();
        println!("{} {} bytes", path.display(), text.len());
    }
}

// ---------------------------------------------------------------- parse: (a) (c) (f)

fn cmd_parse(paths: &[PathBuf]) {
    println!(
        "file\tlang\tMiB\tlines\tnodes\tfull_ms\tMiB/s\trope_ms\tchunk4k_ms\tchunk64k_ms\ttree_MiB\ttree/x\tpeak_MiB\terror"
    );
    for path in paths {
        let text = read(path);
        let lang = lang_for(path);
        let mut p = parser_for(&lang);
        let reps = reps_for(text.len());
        let lines = memchr::memchr_iter(b'\n', text.as_bytes()).count() + 1;

        let (full, tree) = best(reps, || parse_contiguous(&mut p, &text));
        let (_, nodes) = tree_hash(&tree);
        let error = tree.root_node().has_error();
        drop(tree);

        let rope = ropey::Rope::from_str(&text);
        let (rope_t, t) = best(reps, || parse_rope(&mut p, &rope));
        drop(t);
        drop(rope);
        let (c4, t) = best(reps, || parse_chunks(&mut p, &text, 4096));
        drop(t);
        let (c64, t) = best(reps, || parse_chunks(&mut p, &text, 65536));
        drop(t);

        // Memory: the retained tree, and the peak while parsing (sampled from
        // the progress callback, which runs every ~100 parser operations).
        drop(p);
        trim();
        let before = allocated();
        let mut peak = before;
        let mut calls = 0u64;
        let mut p = parser_for(&lang);
        let mut progress = |_: &tree_sitter::ParseState| {
            calls += 1;
            if calls % 256 == 0 {
                peak = peak.max(allocated());
            }
            ControlFlow::Continue(())
        };
        let bytes = text.as_bytes();
        let tree = p
            .parse_with_options(
                &mut |i, _: Point| &bytes[i.min(bytes.len())..],
                None,
                Some(ParseOptions::new().progress_callback(&mut progress)),
            )
            .unwrap();
        peak = peak.max(allocated());
        drop(p);
        trim();
        let tree_bytes = allocated().saturating_sub(before);
        drop(tree);

        println!(
            "{}\t{}\t{:.2}\t{}\t{}\t{:.1}\t{:.2}\t{:.1}\t{:.1}\t{:.1}\t{:.1}\t{:.1}\t{:.1}\t{}",
            path.file_name().unwrap().to_string_lossy(),
            lang.name,
            mib(text.len()),
            lines,
            nodes,
            ms(full),
            mib(text.len()) / full.as_secs_f64(),
            ms(rope_t),
            ms(c4),
            ms(c64),
            mib(tree_bytes),
            tree_bytes as f64 / text.len() as f64,
            mib(peak - before),
            error,
        );
    }
}

// ---------------------------------------------------------------- slice: (b)

fn cmd_slice(paths: &[PathBuf]) {
    println!(
        "file\tbudget_ms\tfull_ms\tslices\tsliced_total_ms\toverhead_%\tmax_slice_ms\tp99_slice_ms\tsame_tree"
    );
    for path in paths {
        let text = read(path);
        let lang = lang_for(path);
        let mut p = parser_for(&lang);
        let (full, tree) = best(reps_for(text.len()), || parse_contiguous(&mut p, &text));
        let want = tree_hash(&tree);
        drop(tree);
        for budget_ms in [2u64, 5, 8] {
            let budget = Duration::from_millis(budget_ms);
            p.reset();
            let bytes = text.as_bytes();
            let mut slices = Vec::new();
            let total_start = Instant::now();
            let tree = loop {
                let start = Instant::now();
                let mut progress = |_: &tree_sitter::ParseState| {
                    if start.elapsed() >= budget {
                        ControlFlow::Break(())
                    } else {
                        ControlFlow::Continue(())
                    }
                };
                // Same input and no reset between calls: the parser resumes.
                let r = p.parse_with_options(
                    &mut |i, _: Point| &bytes[i.min(bytes.len())..],
                    None,
                    Some(ParseOptions::new().progress_callback(&mut progress)),
                );
                slices.push(start.elapsed());
                if let Some(t) = r {
                    break t;
                }
            };
            let total = total_start.elapsed();
            let same = tree_hash(&tree) == want;
            slices.sort();
            let p99 = slices[(slices.len() * 99 / 100).min(slices.len() - 1)];
            println!(
                "{}\t{}\t{:.1}\t{}\t{:.1}\t{:.1}\t{:.2}\t{:.2}\t{}",
                path.file_name().unwrap().to_string_lossy(),
                budget_ms,
                ms(full),
                slices.len(),
                ms(total),
                (total.as_secs_f64() / full.as_secs_f64() - 1.0) * 100.0,
                ms(*slices.last().unwrap()),
                ms(p99),
                same
            );
        }
    }
}

// ---------------------------------------------------------------- highlight helpers

/// Per byte of `range` (absolute offsets): the capture index chosen by the
/// innermost node, earliest pattern on ties; u32::MAX when no capture.
fn capture_map(
    query: &Query,
    tree: &Tree,
    text: &[u8],
    base: usize,
    range: Range<usize>,
) -> Vec<u32> {
    let mut best: Vec<(u32, u32, u32)> = vec![(u32::MAX, u32::MAX, u32::MAX); range.len()];
    let mut cursor = QueryCursor::new();
    cursor.set_byte_range(range.start - base..range.end - base);
    let mut caps = cursor.captures(query, tree.root_node(), text);
    while let Some((m, ix)) = caps.next() {
        let cap = m.captures()[*ix];
        let s = cap.node.start_byte() + base;
        let e = cap.node.end_byte() + base;
        let span = (e - s) as u32;
        let pat = m.pattern_index as u32;
        for b in s.max(range.start)..e.min(range.end) {
            let slot = &mut best[b - range.start];
            if span < slot.0 || (span == slot.0 && pat < slot.1) {
                *slot = (span, pat, cap.index);
            }
        }
    }
    best.into_iter().map(|(_, _, c)| c).collect()
}

fn agreement(a: &[u32], b: &[u32], text: &[u8], range: Range<usize>) -> f64 {
    let mut total = 0usize;
    let mut same = 0usize;
    for (i, byte) in text[range].iter().enumerate() {
        if byte.is_ascii_whitespace() {
            continue;
        }
        total += 1;
        if a[i] == b[i] {
            same += 1;
        }
    }
    if total == 0 {
        1.0
    } else {
        same as f64 / total as f64
    }
}

/// A line that starts at column 0 with something other than a closing
/// bracket or a comment continuation: a likely top-level statement start.
fn is_boundary(line: &str) -> bool {
    match line.as_bytes().first() {
        None => false,
        Some(b' ' | b'\t' | b'}' | b')' | b']' | b'*' | b'\r') => false,
        Some(_) => true,
    }
}

// ---------------------------------------------------------------- window: (d)

const VISIBLE_LINES: usize = 60;

fn indent(line: &str) -> usize {
    line.bytes()
        .take_while(|b| *b == b' ' || *b == b'\t')
        .count()
}

/// Start line for a window that must begin at or before `from`: the nearest
/// column-0 statement start within 2000 lines, else the least-indented
/// non-blank line in that range (JSON and other nested formats).
fn boundary_before<'a>(line_str: &dyn Fn(usize) -> &'a str, from: usize) -> usize {
    let lo = from.saturating_sub(2000);
    let mut best = from;
    let mut best_indent = usize::MAX;
    let mut l = from;
    loop {
        let s = line_str(l);
        if is_boundary(s) {
            return l;
        }
        if !s.trim().is_empty() && indent(s) < best_indent {
            best_indent = indent(s);
            best = l;
        }
        if l == lo {
            return best;
        }
        l -= 1;
    }
}

/// First-screen variant of `boundary_before` with a byte budget: the nearest
/// column-0 statement start among the lines that begin within `budget` bytes
/// before `from`'s start, else the least-indented line there that does not
/// look like a comment continuation.
fn boundary_before_bytes<'a>(
    line_str: &dyn Fn(usize) -> &'a str,
    starts: &[usize],
    from: usize,
    budget: usize,
) -> usize {
    let limit = starts[from].saturating_sub(budget);
    let mut best = from;
    let mut best_indent = usize::MAX;
    let mut l = from;
    loop {
        let s = line_str(l);
        if is_boundary(s) {
            return l;
        }
        let t = s.trim_start();
        let comment = t.starts_with('*') || t.starts_with("//") || t.starts_with("/*");
        if !t.is_empty() && !comment && indent(s) < best_indent {
            best_indent = indent(s);
            best = l;
        }
        if l == 0 || starts[l - 1] < limit {
            return best;
        }
        l -= 1;
    }
}

/// End line (inclusive) for a window that must reach `from`: the line before
/// the next column-0 statement start within 2000 lines, else `from`.
fn boundary_after<'a>(line_str: &dyn Fn(usize) -> &'a str, from: usize, nlines: usize) -> usize {
    let hi = (from + 2000).min(nlines - 1);
    let mut l = from;
    while l < hi {
        if is_boundary(line_str(l + 1)) {
            return l;
        }
        l += 1;
    }
    from
}

/// JSON windows: a window that starts at an object member gets a synthetic
/// `{` in front, so the member parses as a pair. Returns the text to parse
/// and the absolute offset of its first byte.
fn window_text(json: bool, text: &str, ws: usize, we: usize) -> (std::borrow::Cow<'_, str>, usize) {
    let w = &text[ws..we];
    if !json {
        return (std::borrow::Cow::Borrowed(w), ws);
    }
    // Open what the window closes without opening, plus one object around the
    // members at the window's own top level; close what it leaves open.
    let mut unmatched = Vec::new();
    {
        let mut depth = 0usize;
        let (mut in_str, mut esc) = (false, false);
        for c in w.bytes() {
            if in_str {
                if esc {
                    esc = false;
                } else if c == b'\\' {
                    esc = true;
                } else if c == b'"' {
                    in_str = false;
                }
                continue;
            }
            match c {
                b'"' => in_str = true,
                b'{' | b'[' => depth += 1,
                b'}' | b']' if depth == 0 => unmatched.push(if c == b'}' { '{' } else { '[' }),
                b'}' | b']' => depth -= 1,
                _ => {}
            }
        }
    }
    let mut prefix = String::new();
    if ws > 0 {
        prefix.push('{');
        // Each closed-but-not-opened bracket was the value of a member.
        for o in unmatched.iter().rev() {
            prefix.push_str("\"\":");
            prefix.push(*o);
        }
    }
    let (mut out, base) = if !prefix.is_empty() && ws >= prefix.len() {
        (format!("{prefix}{w}"), ws - prefix.len())
    } else {
        (w.to_string(), ws)
    };
    let mut stack = Vec::new();
    let (mut in_str, mut esc) = (false, false);
    for c in out.bytes() {
        if in_str {
            if esc {
                esc = false;
            } else if c == b'\\' {
                esc = true;
            } else if c == b'"' {
                in_str = false;
            }
            continue;
        }
        match c {
            b'"' => in_str = true,
            b'{' => stack.push(b'}'),
            b'[' => stack.push(b']'),
            b'}' | b']' => {
                stack.pop();
            }
            _ => {}
        }
    }
    if in_str {
        out.push('"');
    }
    while let Some(c) = stack.pop() {
        out.push(c as char);
    }
    (std::borrow::Cow::Owned(out), base)
}

/// One-line JSON: the start of the last object member before `from`, found
/// with a lexical scan that tracks whether each byte is inside a string.
/// (A real implementation keeps string-state checkpoints every 64 KiB.)
fn json_boundary_before(text: &str, from: usize) -> usize {
    let b = text.as_bytes();
    let (mut in_str, mut esc, mut last) = (false, false, 0usize);
    for (i, &c) in b[..from.min(b.len())].iter().enumerate() {
        if in_str {
            if esc {
                esc = false;
            } else if c == b'\\' {
                esc = true;
            } else if c == b'"' {
                in_str = false;
            }
        } else if c == b'"' {
            in_str = true;
        } else if c == b',' {
            last = i + 1;
        }
    }
    last
}

/// For one-line files: the offset just after the last `;` or `}` at or
/// before `from` (within 64 KiB), else `from` snapped to a char boundary.
fn byte_boundary_before(text: &str, from: usize) -> usize {
    let b = text.as_bytes();
    let lo = from.saturating_sub(64 << 10);
    let mut i = from.min(b.len());
    while i > lo {
        if b[i - 1] == b';' || b[i - 1] == b'}' {
            return i;
        }
        i -= 1;
    }
    let mut i = from;
    while !text.is_char_boundary(i) {
        i -= 1;
    }
    i
}

fn snap(text: &str, mut i: usize) -> usize {
    i = i.min(text.len());
    while !text.is_char_boundary(i) {
        i += 1;
    }
    i
}

fn cmd_window(paths: &[PathBuf]) {
    println!(
        "file\tmode\ttargets\twindow_KiB\tparse_ms_mean\tparse_ms_max\tquery_ms_mean\tagree_mean\tagree_min\tperfect_%"
    );
    for path in paths {
        let text = read(path);
        let bytes = text.as_bytes();
        let lang = lang_for(path);
        let query = query_for(&lang);
        let mut p = parser_for(&lang);
        let full = parse_contiguous(&mut p, &text);
        let starts = line_starts(&text);
        let nlines = starts.len();
        let line_end = |l: usize| {
            if l + 1 < nlines {
                starts[l + 1]
            } else {
                text.len()
            }
        };
        let line_str = |l: usize| &text[starts[l]..line_end(l)];
        let one_line = nlines < 2 * VISIBLE_LINES;

        // Targets: the file start, then 40 positions spread over 5%..95%
        // (lines; byte offsets for files with fewer lines than two screens).
        let mut targets = vec![0usize];
        let mut rng = fixtures::Rng::new(42);
        let span = if one_line { text.len() } else { nlines };
        for _ in 0..40 {
            let lo = span / 20;
            let hi = span * 19 / 20;
            targets.push(lo + rng.below((hi - lo).max(1) as u64) as usize);
        }

        let line_of = |off: usize| starts.partition_point(|&s| s <= off) - 1;
        for mode in ["start16k", "start64k", "first", "boundary", "naive"] {
            let mut parse_t = Vec::new();
            let mut query_t = Vec::new();
            let mut agree = Vec::new();
            let mut win_bytes = 0usize;
            let mut n = 0usize;
            for &t in &targets {
                if mode.starts_with("start") != (t == 0) {
                    continue;
                }
                // Visible: 60 lines, or 8 KiB of a one-line file.
                let vis = if one_line {
                    snap(&text, t)..snap(&text, t + 8192)
                } else {
                    starts[t]..line_end((t + VISIBLE_LINES).min(nlines - 1))
                };
                let (ws, we) = if mode.starts_with("start") {
                    let want = if mode == "start16k" {
                        16 << 10
                    } else {
                        64 << 10
                    };
                    let mut end = want.min(text.len());
                    if !one_line {
                        while end < text.len() && bytes[end - 1] != b'\n' {
                            end += 1;
                        }
                    }
                    (0, snap(&text, end.max(vis.end)))
                } else if one_line {
                    // 32 KiB of context before, 16 KiB after the visible part
                    // (first screen: 8 KiB before, 4 KiB after).
                    let (before, after) = if mode == "first" {
                        (8 << 10, 4 << 10)
                    } else {
                        (32 << 10, 16 << 10)
                    };
                    let s0 = t.saturating_sub(before);
                    let s = match (mode, lang.name) {
                        ("boundary" | "first", "json") => json_boundary_before(&text, s0),
                        ("boundary" | "first", _) => byte_boundary_before(&text, s0),
                        _ => snap(&text, s0),
                    };
                    (s, snap(&text, vis.end + after))
                } else {
                    // 200 lines of context before the target, 100 after the
                    // visible lines.
                    let vis_end_line = (t + VISIBLE_LINES).min(nlines - 1);
                    let s0 = t.saturating_sub(200);
                    let e0 = (vis_end_line + 100).min(nlines - 1);
                    let (s, e) = if mode == "boundary" {
                        (
                            boundary_before(&line_str, s0),
                            boundary_after(&line_str, e0, nlines),
                        )
                    } else if mode == "first" {
                        // First screen mid-file: about 8 KiB of context
                        // (boundary within 4 KiB more), 4 KiB after.
                        let c = line_of(vis.start.saturating_sub(8 << 10));
                        let e = line_of((vis.end + (4 << 10)).min(text.len() - 1));
                        (boundary_before_bytes(&line_str, &starts, c, 4 << 10), e)
                    } else {
                        (s0, e0)
                    };
                    (starts[s], line_end(e))
                };
                if vis.end > we || vis.start < ws {
                    continue;
                }
                let (window, base) =
                    window_text(lang.name == "json" && mode != "naive", &text, ws, we);
                let start = Instant::now();
                p.reset();
                let wt = p.parse(window.as_ref(), None).unwrap();
                parse_t.push(start.elapsed());
                let qs = Instant::now();
                let got = capture_map(&query, &wt, window.as_bytes(), base, vis.clone());
                query_t.push(qs.elapsed());
                let want = capture_map(&query, &full, bytes, 0, vis.clone());
                let a = agreement(&got, &want, bytes, vis.clone());
                if std::env::var_os("LFR_DEBUG").is_some() && a < 0.99 {
                    eprintln!(
                        "{mode} target {t} agree {a:.3}: window starts {:?}, visible starts {:?}",
                        &window[..window.len().min(80)],
                        &text[vis.start..(vis.start + 60).min(text.len())]
                    );
                }
                agree.push(a);
                win_bytes += we - ws;
                n += 1;
            }
            if n == 0 {
                continue;
            }
            let mean = |v: &[Duration]| v.iter().map(|d| ms(*d)).sum::<f64>() / v.len() as f64;
            let maxd = |v: &[Duration]| v.iter().map(|d| ms(*d)).fold(0.0, f64::max);
            let perfect = agree.iter().filter(|&&a| a >= 0.999_999).count();
            println!(
                "{}\t{}\t{}\t{:.0}\t{:.2}\t{:.2}\t{:.2}\t{:.4}\t{:.4}\t{:.0}",
                path.file_name().unwrap().to_string_lossy(),
                mode,
                n,
                win_bytes as f64 / n as f64 / 1024.0,
                mean(&parse_t),
                maxd(&parse_t),
                mean(&query_t),
                agree.iter().sum::<f64>() / n as f64,
                agree.iter().cloned().fold(1.0, f64::min),
                perfect as f64 * 100.0 / n as f64,
            );
        }
    }
}

// ---------------------------------------------------------------- chunked: whole file in windows

/// Highlights the whole file as consecutive windows of about `kib` KiB cut at
/// statement boundaries, each parsed on its own and dropped, and compares the
/// result with the full parse over every non-whitespace byte.
/// The end of the last error-free node that closes before the window's end,
/// at the shallowest depth that has one past `min`: descend into the node
/// the window cuts through until a complete earlier sibling is found.
fn last_complete_end(tree: &Tree, len: usize, min: usize) -> Option<usize> {
    let mut node = tree.root_node();
    loop {
        let mut cursor = node.walk();
        let children: Vec<tree_sitter::Node> = node.children(&mut cursor).collect();
        let cut_ix = children
            .iter()
            .position(|c| c.end_byte() + 1 >= len || c.has_error())
            .unwrap_or(children.len());
        if let Some(c) = children[..cut_ix]
            .iter()
            .rev()
            .find(|c| !c.has_error() && c.end_byte() >= min)
        {
            return Some(c.end_byte());
        }
        let next = children.get(cut_ix)?;
        if next.child_count() == 0 {
            return None;
        }
        node = *next;
    }
}

fn cmd_chunked(kib: usize, tree_cut: bool, paths: &[PathBuf]) {
    println!(
        "file\twindow_KiB\twindows\tmax_window_KiB\tsum_parse_ms\tfull_parse_ms\tmax_window_parse_ms\tmax_window_tree_MiB\tagree\tworst_window_agree"
    );
    let w = kib << 10;
    for path in paths {
        let text = read(path);
        let bytes = text.as_bytes();
        let lang = lang_for(path);
        let query = query_for(&lang);
        let mut p = parser_for(&lang);
        let t0 = Instant::now();
        let full = parse_contiguous(&mut p, &text);
        let full_t = t0.elapsed();
        let starts = line_starts(&text);
        let nlines = starts.len();
        let one_line = nlines < 2 * VISIBLE_LINES;
        let line_of = |off: usize| starts.partition_point(|&s| s <= off) - 1;
        let line_end = |l: usize| {
            if l + 1 < nlines {
                starts[l + 1]
            } else {
                text.len()
            }
        };
        let line_str = |l: usize| &text[starts[l]..line_end(l)];

        let mut s = 0usize;
        let (mut windows, mut max_win, mut sum_parse, mut max_parse, mut max_tree) =
            (0usize, 0usize, Duration::ZERO, Duration::ZERO, 0usize);
        let (mut total, mut same) = (0usize, 0usize);
        let mut worst = 1.0f64;
        while s < text.len() {
            let e = if s + w >= text.len() {
                text.len()
            } else if one_line {
                let b = if lang.name == "json" {
                    json_boundary_before(&text, s + w)
                } else {
                    byte_boundary_before(&text, s + w)
                };
                if b > s + w / 2 { b } else { snap(&text, s + w) }
            } else {
                let l = line_of(s + w);
                line_end(boundary_after(&line_str, l, nlines))
            };
            let (window, base) = window_text(lang.name == "json", &text, s, e);
            trim();
            let before = allocated();
            let t = Instant::now();
            p.reset();
            let wt = p.parse(window.as_ref(), None).unwrap();
            let d = t.elapsed();
            max_tree = max_tree.max(allocated().saturating_sub(before));
            sum_parse += d;
            max_parse = max_parse.max(d);
            max_win = max_win.max(e - s);
            // Tree cut: keep only the part up to the end of the last complete
            // node before the window's end; the next window starts there.
            let e = if tree_cut && e < text.len() {
                let len = e - base;
                match last_complete_end(&wt, len, len / 4) {
                    Some(c) => (base + c).min(e),
                    None => e,
                }
            } else {
                e
            };
            let got = capture_map(&query, &wt, window.as_bytes(), base, s..e);
            drop(wt);
            let want = capture_map(&query, &full, bytes, 0, s..e);
            let (mut wt_total, mut wt_same) = (0usize, 0usize);
            for (i, byte) in bytes[s..e].iter().enumerate() {
                if byte.is_ascii_whitespace() {
                    continue;
                }
                wt_total += 1;
                if got[i] == want[i] {
                    wt_same += 1;
                } else if std::env::var_os("LFR_DEBUG").is_some() && windows < 2 {
                    let name = |c: u32| {
                        query
                            .capture_names()
                            .get(c as usize)
                            .copied()
                            .unwrap_or("-")
                    };
                    let at = s + i;
                    eprintln!(
                        "diff at {at}: window {} full {} near {:?}",
                        name(got[i]),
                        name(want[i]),
                        &text[at.saturating_sub(20).max(s)..(at + 20).min(e)]
                    );
                }
            }
            total += wt_total;
            same += wt_same;
            if wt_total > 0 {
                worst = worst.min(wt_same as f64 / wt_total as f64);
            }
            windows += 1;
            s = e;
        }
        println!(
            "{}\t{}\t{}\t{:.0}\t{:.0}\t{:.0}\t{:.1}\t{:.1}\t{:.5}\t{:.4}",
            path.file_name().unwrap().to_string_lossy(),
            if tree_cut {
                format!("{kib}/tree")
            } else {
                kib.to_string()
            },
            windows,
            max_win as f64 / 1024.0,
            ms(sum_parse),
            ms(full_t),
            ms(max_parse),
            mib(max_tree),
            same as f64 / total.max(1) as f64,
            worst
        );
    }
}

// ---------------------------------------------------------------- first: bounded first screen

/// Total backward scan allowed when choosing the first-screen window.
const FIRST_BACK_CAP: usize = 64 << 10;

/// Start of the line containing `pos`, if it begins within `limit` bytes.
fn line_start_within(b: &[u8], pos: usize, limit: usize) -> Option<usize> {
    let lo = pos.saturating_sub(limit);
    if pos == 0 {
        return Some(0);
    }
    match memchr::memrchr(b'\n', &b[lo..pos]) {
        Some(i) => Some(lo + i + 1),
        None if lo == 0 => Some(0),
        None => None,
    }
}

/// One-line JSON without knowing the string state: the nearest `,"key":`
/// before `from` whose key has no backslash and is at most 256 bytes.
fn json_key_boundary_back(b: &[u8], from: usize, lo: usize) -> Option<usize> {
    let mut i = from;
    while i > lo + 1 {
        i -= 1;
        if b[i] == b'"' && b[i - 1] == b',' {
            let rest = &b[i + 1..(i + 258).min(b.len())];
            if let Some(q) = memchr::memchr(b'"', rest) {
                if !rest[..q].contains(&b'\\') && rest.get(q + 1) == Some(&b':') {
                    return Some(i);
                }
            }
        }
    }
    None
}

/// Chooses the first-screen window for a target byte offset using only the
/// bytes near it (at most `FIRST_BACK_CAP` before, 64 KiB after): no line
/// index, no scan from the file start. None: no safe start within the cap,
/// so the first screen is plain text and the boundary is found later.
fn first_window(text: &str, json: bool, x: usize) -> Option<(usize, usize, Range<usize>)> {
    let b = text.as_bytes();
    let floor = x.saturating_sub(FIRST_BACK_CAP);
    if let Some(ls) = line_start_within(b, x, 4096) {
        // Line mode: 60 lines forward (at most 64 KiB), context ~8 KiB back
        // plus up to 4 KiB more for a statement start.
        let mut end = ls;
        for _ in 0..=VISIBLE_LINES {
            let lim = (ls + (64 << 10)).min(b.len());
            match memchr::memchr(b'\n', &b[end.min(lim)..lim]) {
                Some(i) => end = end + i + 1,
                None => {
                    end = lim;
                    break;
                }
            }
        }
        let vis = ls..end;
        let c = line_start_within(b, ls.saturating_sub(8 << 10).max(floor), 4096)?;
        let limit = c.saturating_sub(4 << 10).max(floor);
        let (mut best, mut best_indent, mut l) = (c, usize::MAX, c);
        let start = loop {
            let le = memchr::memchr(b'\n', &b[l..]).map_or(b.len(), |i| l + i + 1);
            let s = &text[l..le];
            if is_boundary(s) {
                break l;
            }
            let t = s.trim_start();
            let comment = t.starts_with('*') || t.starts_with("//") || t.starts_with("/*");
            if !t.is_empty() && !comment && indent(s) < best_indent {
                best_indent = indent(s);
                best = l;
            }
            if l == 0 || l <= limit {
                break best;
            }
            match line_start_within(b, l - 1, l - limit) {
                Some(p) if p >= limit => l = p,
                _ => break best,
            }
        };
        let tail = (end + (4 << 10)).min(b.len());
        let we = memchr::memchr(b'\n', &b[tail..(tail + 4096).min(b.len())])
            .map_or(tail, |i| tail + i + 1);
        Some((start, snap(text, we), vis))
    } else {
        // Long line: 8 KiB visible, 8 KiB of context, safe start within the cap.
        let vis = snap(text, x)..snap(text, x + 8192);
        let s0 = x.saturating_sub(8 << 10);
        let start = if json {
            json_key_boundary_back(b, s0, floor)?
        } else {
            let mut i = s0;
            loop {
                if i <= floor {
                    return None;
                }
                if b[i - 1] == b';' || b[i - 1] == b'}' {
                    break i;
                }
                i -= 1;
            }
        };
        Some((start, snap(text, vis.end + (4 << 10)), vis))
    }
}

/// First screen at 40 random byte offsets, with the window choice, JSON
/// balancing, parse and visible-range query all inside the timed region.
/// Agreement is skipped for files listed after `--no-ref`.
fn cmd_first(paths: &[PathBuf]) {
    println!(
        "file\ttargets\tfallback\twindow_KiB_mean\tselect_ms_max\ttotal_ms_mean\ttotal_ms_p95\ttotal_ms_max\tagree_mean\tagree_min\tperfect_%"
    );
    let mut with_ref = true;
    for path in paths {
        if path.as_os_str() == "--no-ref" {
            with_ref = false;
            continue;
        }
        let text = read(path);
        let bytes = text.as_bytes();
        let lang = lang_for(path);
        let json = lang.name == "json";
        let query = query_for(&lang);
        let mut p = parser_for(&lang);
        let full = with_ref.then(|| parse_contiguous(&mut p, &text));
        let mut rng = fixtures::Rng::new(42);
        let (mut totals, mut selects, mut agree) = (Vec::new(), Vec::new(), Vec::new());
        let (mut fallback, mut win) = (0usize, 0usize);
        for _ in 0..40 {
            let lo = text.len() / 20;
            let x = snap(&text, lo + rng.below((text.len() * 9 / 10) as u64) as usize);
            let t0 = Instant::now();
            let Some((ws, we, vis)) = first_window(&text, json, x) else {
                selects.push(t0.elapsed());
                fallback += 1;
                continue;
            };
            selects.push(t0.elapsed());
            let (window, base) = window_text(json, &text, ws, we);
            p.reset();
            let wt = p.parse(window.as_ref(), None).unwrap();
            let got = capture_map(&query, &wt, window.as_bytes(), base, vis.clone());
            totals.push(t0.elapsed());
            win += we - ws;
            if let Some(full) = &full {
                let want = capture_map(&query, full, bytes, 0, vis.clone());
                agree.push(agreement(&got, &want, bytes, vis));
            }
        }
        totals.sort();
        let n = totals.len().max(1);
        let mean = totals.iter().map(|d| ms(*d)).sum::<f64>() / n as f64;
        let p95 = totals.get(totals.len() * 95 / 100).map_or(0.0, |d| ms(*d));
        let fmt_agree = |v: &[f64]| {
            if v.is_empty() {
                ("-".to_string(), "-".to_string(), "-".to_string())
            } else {
                (
                    format!("{:.4}", v.iter().sum::<f64>() / v.len() as f64),
                    format!("{:.4}", v.iter().cloned().fold(1.0, f64::min)),
                    format!(
                        "{:.0}",
                        v.iter().filter(|&&a| a >= 0.999_999).count() as f64 * 100.0
                            / v.len() as f64
                    ),
                )
            }
        };
        let (am, amin, perf) = fmt_agree(&agree);
        println!(
            "{}\t{}\t{}\t{:.0}\t{:.3}\t{:.2}\t{:.2}\t{:.2}\t{}\t{}\t{}",
            path.file_name().unwrap().to_string_lossy(),
            40,
            fallback,
            win as f64 / n as f64 / 1024.0,
            selects.iter().map(|d| ms(*d)).fold(0.0, f64::max),
            mean,
            p95,
            totals.last().map_or(0.0, |d| ms(*d)),
            am,
            amin,
            perf
        );
    }
}

// ---------------------------------------------------------------- query: (e)

fn cmd_query(paths: &[PathBuf]) {
    println!("file\twhole_ms\twhole_captures\tvis_start_ms\tvis_mid_ms\tvis_end_ms\tvis_captures");
    for path in paths {
        let text = read(path);
        let bytes = text.as_bytes();
        let lang = lang_for(path);
        let query = query_for(&lang);
        let mut p = parser_for(&lang);
        let tree = parse_contiguous(&mut p, &text);
        let starts = line_starts(&text);
        let nlines = starts.len();
        let line_end = |l: usize| {
            if l + 1 < nlines {
                starts[l + 1]
            } else {
                text.len()
            }
        };

        let count = |range: Range<usize>| {
            let mut cursor = QueryCursor::new();
            cursor.set_byte_range(range);
            let mut caps = cursor.captures(&query, tree.root_node(), bytes);
            let mut n = 0usize;
            while let Some((m, ix)) = caps.next() {
                black_box(m.captures()[*ix].node.start_byte());
                n += 1;
            }
            n
        };
        let reps = reps_for(text.len());
        let (whole, whole_n) = best(reps, || count(0..text.len()));
        let mut vis = Vec::new();
        let mut vis_n = 0;
        for frac in [0usize, 50, 99] {
            let first = (nlines * frac / 100).min(nlines.saturating_sub(1));
            let last = (first + VISIBLE_LINES).min(nlines - 1);
            // For a one-line file the "visible" range is the first 8 KiB.
            let mut range = starts[first]..line_end(last);
            if nlines < VISIBLE_LINES {
                let s = text.len() * frac / 100;
                range = s..(s + 8192).min(text.len());
            }
            let (d, n) = best(20, || count(range.clone()));
            vis.push(d);
            vis_n = n;
        }
        println!(
            "{}\t{:.1}\t{}\t{:.3}\t{:.3}\t{:.3}\t{}",
            path.file_name().unwrap().to_string_lossy(),
            ms(whole),
            whole_n,
            ms(vis[0]),
            ms(vis[1]),
            ms(vis[2]),
            vis_n
        );
    }
}

// ---------------------------------------------------------------- text: rope, line index, hashing

fn blob_oid(bytes: &[u8]) -> [u8; 20] {
    let mut h = sha1dc::Hasher::default();
    h.update(format!("blob {}\0", bytes.len()).as_bytes());
    h.update(bytes);
    h.finalize().expect("no collision").into()
}

fn cmd_text(paths: &[PathBuf]) {
    println!(
        "file\tMiB\tlines\tline_index_ms\trope_from_ms\trope_MiB\tline_walk_ms\tsha1dc_ms\tsha1dc_MiB/s"
    );
    for path in paths {
        let text = read(path);
        let reps = reps_for(text.len());
        let (li, starts) = best(reps, || line_starts(&text));
        trim();
        let before = allocated();
        let (rf, rope) = best(1, || ropey::Rope::from_str(&text));
        let rope_bytes = allocated().saturating_sub(before);
        // What a whole-document line pass costs (the shape of gpui-base's
        // wrapper update with wrapping off): visit every line as a slice.
        let (lw, total) = best(reps, || {
            let mut total = 0usize;
            for line in rope.lines(ropey::LineType::LF) {
                total += line.len();
            }
            total
        });
        black_box(total);
        let (hs, oid) = best(reps, || blob_oid(text.as_bytes()));
        black_box(oid);
        println!(
            "{}\t{:.2}\t{}\t{:.2}\t{:.1}\t{:.1}\t{:.1}\t{:.1}\t{:.0}",
            path.file_name().unwrap().to_string_lossy(),
            mib(text.len()),
            starts.len(),
            ms(li),
            ms(rf),
            mib(rope_bytes),
            ms(lw),
            ms(hs),
            mib(text.len()) / hs.as_secs_f64(),
        );
    }
}

/// Walk a directory without following symlinks: a stat-only pass, then a
/// read + blob OID pass. Prints file count, bytes and both times.
fn cmd_hash(dir: &Path) {
    fn walk(dir: &Path, files: &mut Vec<PathBuf>) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for e in rd.flatten() {
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_dir() {
                walk(&e.path(), files);
            } else if ft.is_file() {
                files.push(e.path());
            }
        }
    }
    let t = Instant::now();
    let mut files = Vec::new();
    walk(dir, &mut files);
    let list = t.elapsed();

    let t = Instant::now();
    let mut bytes = 0u64;
    for f in &files {
        if let Ok(m) = std::fs::symlink_metadata(f) {
            bytes += m.len();
            black_box(m.modified().ok());
        }
    }
    let stat = t.elapsed();

    let t = Instant::now();
    for f in &files {
        if let Ok(b) = std::fs::read(f) {
            black_box(blob_oid(&b));
        }
    }
    let hash = t.elapsed();
    println!("dir\tfiles\tMiB\tlist_ms\tstat_ms\tstat_us_per_file\tread_hash_ms\tread_hash_MiB/s");
    println!(
        "{}\t{}\t{:.1}\t{:.0}\t{:.0}\t{:.2}\t{:.0}\t{:.0}",
        dir.display(),
        files.len(),
        mib(bytes as usize),
        ms(list),
        ms(stat),
        stat.as_secs_f64() * 1e6 / files.len().max(1) as f64,
        ms(hash),
        mib(bytes as usize) / hash.as_secs_f64(),
    );
}

// ---------------------------------------------------------------- contend

/// A fixed amount of "frame work" on the main thread every 8.33 ms while
/// 0..=3 threads parse continuously: how much the frame work slows down.
fn cmd_contend(path: &Path) {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    let text = Arc::new(read(path));
    let mut buf = vec![0u64; 1 << 18]; // 2 MiB working set
    let frame = |buf: &mut Vec<u64>| {
        let mut acc = 0u64;
        for _ in 0..6 {
            for v in buf.iter_mut() {
                *v = v.wrapping_mul(6364136223846793005).wrapping_add(acc);
                acc ^= *v >> 7;
            }
        }
        black_box(acc)
    };
    println!("parsers\tframes\tframe_p50_ms\tframe_p99_ms\tframe_max_ms");
    for threads in 0..=3usize {
        let stop = Arc::new(AtomicBool::new(false));
        let mut handles = Vec::new();
        for _ in 0..threads {
            let stop = stop.clone();
            let text = text.clone();
            let path = path.to_path_buf();
            handles.push(std::thread::spawn(move || {
                let lang = lang_for(&path);
                let mut p = parser_for(&lang);
                while !stop.load(Ordering::Relaxed) {
                    let bytes = text.as_bytes();
                    let stop2 = stop.clone();
                    let mut progress = |_: &tree_sitter::ParseState| {
                        if stop2.load(Ordering::Relaxed) {
                            ControlFlow::Break(())
                        } else {
                            ControlFlow::Continue(())
                        }
                    };
                    p.reset();
                    black_box(p.parse_with_options(
                        &mut |i, _: Point| &bytes[i.min(bytes.len())..],
                        None,
                        Some(ParseOptions::new().progress_callback(&mut progress)),
                    ));
                }
            }));
        }
        std::thread::sleep(Duration::from_millis(300));
        let mut times = Vec::new();
        let period = Duration::from_micros(8333);
        let mut next = Instant::now();
        for _ in 0..600 {
            let s = Instant::now();
            frame(&mut buf);
            times.push(s.elapsed());
            next += period;
            if let Some(w) = next.checked_duration_since(Instant::now()) {
                std::thread::sleep(w);
            }
        }
        stop.store(true, Ordering::Relaxed);
        for h in handles {
            h.join().unwrap();
        }
        times.sort();
        println!(
            "{}\t{}\t{:.2}\t{:.2}\t{:.2}",
            threads,
            times.len(),
            ms(times[times.len() / 2]),
            ms(times[times.len() * 99 / 100]),
            ms(*times.last().unwrap())
        );
    }
}

// ---------------------------------------------------------------- main

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let paths: Vec<PathBuf> = args.iter().skip(2).map(PathBuf::from).collect();
    match args.get(1).map(String::as_str) {
        Some("gen") => cmd_gen(&paths[0]),
        Some("gen-oneline") => cmd_gen_oneline(&paths[0]),
        Some("first") => cmd_first(&paths),
        Some("parse") => cmd_parse(&paths),
        Some("slice") => cmd_slice(&paths),
        Some("window") => cmd_window(&paths),
        Some("query") => cmd_query(&paths),
        Some("text") => cmd_text(&paths),
        Some("hash") => cmd_hash(&paths[0]),
        Some("contend") => cmd_contend(&paths[0]),
        Some("chunked") => cmd_chunked(args[2].parse().expect("window KiB"), false, &paths[1..]),
        Some("chunked-tree") => {
            cmd_chunked(args[2].parse().expect("window KiB"), true, &paths[1..])
        }
        _ => {
            eprintln!(
                "usage: large-files-rendering-bench gen|parse|slice|window|chunked <KiB>|query|text|hash|contend <paths>"
            );
            std::process::exit(2);
        }
    }
}
