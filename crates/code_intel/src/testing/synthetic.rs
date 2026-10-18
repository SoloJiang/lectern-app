//! Synthetic repositories (「性能」's fixtures): a language mix and
//! repository-shaped source files of about 8 KiB, each with its own names, so
//! that every file is a different blob. Nothing here reads the network or
//! real code.

use std::fmt::Write as _;

use lectern_services::project::{BlobId, FileKind, FileList};

/// The language mix of a synthetic repository, by extension: about one
/// file in ten is not source (manifests, docs), as in a real monorepo.
const MIX: &[(&str, u32)] = &[
    ("ts", 30),
    ("tsx", 10),
    ("js", 12),
    ("java", 14),
    ("rs", 8),
    ("py", 10),
    ("go", 6),
    ("json", 6),
    ("md", 4),
];

/// The extension of synthetic file `i`.
pub fn extension(i: usize) -> &'static str {
    let total: u32 = MIX.iter().map(|(_, w)| w).sum();
    // A fixed permutation so that neighbouring files differ.
    let mut slot = ((i as u64).wrapping_mul(2_654_435_761) % u64::from(total)) as u32;
    for (ext, weight) in MIX {
        if slot < *weight {
            return ext;
        }
        slot -= weight;
    }
    "md"
}

/// The path of synthetic file `i` of `count`: `packages/pkg<p>/src/<dir>/
/// file<i>.<ext>`, about 40 files per directory and 25 directories per
/// package.
pub fn path(i: usize) -> String {
    let dir = i / 40;
    let package = dir / 25;
    format!("packages/pkg{package:03}/src/mod{:02}/file{i:06}.{}", dir % 25, extension(i))
}

/// The package directories of a repository of `count` files.
pub fn package_dirs(count: usize) -> Vec<String> {
    let packages = count.div_ceil(40 * 25);
    (0..packages).map(|p| format!("packages/pkg{p:03}")).collect()
}

/// A synthetic OID for file `i` (no content behind it).
pub fn oid(i: usize) -> BlobId {
    let mut b = [0u8; 20];
    let mixed = (i as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15);
    b[..8].copy_from_slice(&mixed.to_be_bytes());
    b[8..16].copy_from_slice(&(i as u64).to_be_bytes());
    b[19] = 0xa5;
    BlobId::Sha1(b)
}

/// The file list of a synthetic repository of `count` files with synthetic
/// OIDs (`code_intel/map_100k`).
pub fn file_list(count: usize) -> FileList {
    let mut paths: Vec<(String, usize)> = (0..count).map(|i| (path(i), i)).collect();
    paths.sort();
    let mut b = FileList::builder();
    for (p, i) in paths {
        b.push_with_oid(&p, FileKind::File, oid(i));
    }
    b.build()
}

/// About `len` bytes of source for synthetic file `i` with extension `ext`:
/// a module of a few types and many functions with bodies that call
/// functions of other files, the definition and reference density of
/// ordinary code. Non-source extensions get JSON or Markdown.
pub fn source(ext: &str, i: usize, len: usize) -> Vec<u8> {
    let mut out = String::with_capacity(len + 1024);
    let other = (i * 7 + 3) % 997;
    match ext {
        "ts" | "tsx" => ts(&mut out, i, other, len),
        "js" => js(&mut out, i, other, len),
        "java" => java(&mut out, i, other, len),
        "rs" => rust(&mut out, i, other, len),
        "py" => python(&mut out, i, other, len),
        "go" => go(&mut out, i, other, len),
        "json" => {
            let _ = writeln!(out, "{{\"name\": \"pkg-{i}\", \"version\": \"1.0.{other}\"}}");
        }
        _ => {
            let _ = writeln!(out, "# Module {i}\n");
            while out.len() < len.min(2048) {
                out.push_str("Notes about this module and how to use it.\n");
            }
        }
    }
    out.into_bytes()
}

fn ts(out: &mut String, i: usize, o: usize, len: usize) {
    let _ = write!(
        out,
        "import {{ helper{o}, Config{o}, defaultConfig{o} }} from \"../mod/file{o}\";\n\n\
         export interface Options{i} {{\n  name: string;\n  limit: number;\n  scale: number;\n}}\n\n\
         export class Service{i} {{\n  private items: Array<{{ size: number; name: string }}> = [];\n\n  \
         constructor(private readonly config: Config{o}) {{}}\n\n  \
         add(item: {{ size: number; name: string }}): void {{\n    \
         if (item.size > this.config.limit) {{\n      throw new Error(`too large: ${{item.name}}`);\n    }}\n    \
         this.items.push(item);\n  }}\n\n  \
         total(): number {{\n    let sum = 0;\n    for (const item of this.items) {{\n      \
         sum += helper{o}(item.size, this.config.scale);\n    }}\n    return sum;\n  }}\n}}\n\n"
    );
    let mut k = 0;
    while out.len() < len {
        let _ = write!(
            out,
            "export function build{i}_{k}(options: Options{i}, values: number[]): Service{i} {{\n  \
             const service = new Service{i}(defaultConfig{o}());\n  \
             for (const value of values) {{\n    \
             if (value < options.limit) {{\n      \
             service.add({{ size: value * options.scale, name: options.name }});\n    }}\n  }}\n  \
             return service;\n}}\n\n"
        );
        k += 1;
    }
}

fn js(out: &mut String, i: usize, o: usize, len: usize) {
    let _ = write!(
        out,
        "import {{ helper{o}, makeStore{o} }} from \"../mod/file{o}.js\";\n\n\
         export class Store{i} {{\n  constructor(options) {{\n    this.options = options;\n    \
         this.items = new Map();\n  }}\n\n  get(key) {{\n    return this.items.get(key);\n  }}\n\n  \
         put(key, value) {{\n    if (value == null) {{\n      return false;\n    }}\n    \
         this.items.set(key, helper{o}(value, this.options.scale));\n    return true;\n  }}\n}}\n\n"
    );
    let mut k = 0;
    while out.len() < len {
        let _ = write!(
            out,
            "export function load{i}_{k}(entries, options) {{\n  \
             const store = new Store{i}(options);\n  const backing = makeStore{o}(options);\n  \
             for (const [key, value] of entries) {{\n    \
             if (!store.put(key, value)) {{\n      backing.missing(key);\n    }}\n  }}\n  \
             return store;\n}}\n\n"
        );
        k += 1;
    }
}

fn java(out: &mut String, i: usize, o: usize, len: usize) {
    let _ = write!(
        out,
        "package com.example.mod{o};\n\nimport java.util.ArrayList;\nimport java.util.List;\n\n\
         public class Inventory{i} {{\n    private final List<Item{o}> items = new ArrayList<>();\n    \
         private final Pricing{o} pricing;\n\n    \
         public Inventory{i}(Pricing{o} pricing) {{\n        this.pricing = pricing;\n    }}\n\n"
    );
    let mut k = 0;
    while out.len() + 2 < len {
        let _ = write!(
            out,
            "    public long total{k}(int limit) {{\n        long sum = 0;\n        \
             for (Item{o} item : items) {{\n            if (item.size() < limit) {{\n                \
             sum += pricing.priceOf(item, limit);\n            }}\n        }}\n        \
             return sum;\n    }}\n\n"
        );
        k += 1;
    }
    out.push_str("}\n");
}

fn rust(out: &mut String, i: usize, o: usize, len: usize) {
    let _ = write!(
        out,
        "use crate::mod{o}::{{helper{o}, Config{o}}};\n\n\
         pub struct Queue{i} {{\n    items: Vec<u64>,\n    config: Config{o},\n}}\n\n\
         impl Queue{i} {{\n    pub fn new(config: Config{o}) -> Self {{\n        \
         Queue{i} {{ items: Vec::new(), config }}\n    }}\n\n    \
         pub fn push(&mut self, value: u64) -> bool {{\n        if value > self.config.limit {{\n            \
         return false;\n        }}\n        self.items.push(helper{o}(value));\n        true\n    }}\n}}\n\n"
    );
    let mut k = 0;
    while out.len() < len {
        let _ = write!(
            out,
            "pub fn drain{i}_{k}(queue: &mut Queue{i}, limit: u64) -> u64 {{\n    \
             let mut total = 0;\n    for value in queue.items.drain(..) {{\n        \
             if value < limit {{\n            total += helper{o}(value);\n        }}\n    }}\n    \
             total\n}}\n\n"
        );
        k += 1;
    }
}

fn python(out: &mut String, i: usize, o: usize, len: usize) {
    let _ = write!(
        out,
        "from .file{o} import helper{o}, Config{o}\n\n\n\
         class Invoice{i}:\n    def __init__(self, config: Config{o}):\n        \
         self.config = config\n        self.lines = []\n\n    \
         def add(self, amount, label):\n        if amount > self.config.limit:\n            \
         raise ValueError(label)\n        self.lines.append(helper{o}(amount))\n\n\n"
    );
    let mut k = 0;
    while out.len() < len {
        let _ = write!(
            out,
            "def total{i}_{k}(invoice, rate):\n    result = 0\n    \
             for line in invoice.lines:\n        if line > rate:\n            \
             result += helper{o}(line) * rate\n    return result\n\n\n"
        );
        k += 1;
    }
}

fn go(out: &mut String, i: usize, o: usize, len: usize) {
    let _ = write!(
        out,
        "package mod{o}\n\nimport \"example.com/mod{o}/util\"\n\n\
         type Meter{i} struct {{\n\tValue int\n\tLimit int\n}}\n\n\
         func (m *Meter{i}) Add(v int) bool {{\n\tif v > m.Limit {{\n\t\treturn false\n\t}}\n\t\
         m.Value += util.Scale{o}(v)\n\treturn true\n}}\n\n"
    );
    let mut k = 0;
    while out.len() < len {
        let _ = write!(
            out,
            "func Sum{i}_{k}(meters []*Meter{i}, limit int) int {{\n\ttotal := 0\n\t\
             for _, m := range meters {{\n\t\tif m.Value < limit {{\n\t\t\t\
             total += util.Scale{o}(m.Value)\n\t\t}}\n\t}}\n\treturn total\n}}\n\n"
        );
        k += 1;
    }
}
