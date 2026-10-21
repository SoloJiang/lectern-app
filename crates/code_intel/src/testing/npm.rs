//! Synthetic npm trees for the dependency tier's tests and benchmarks
//! (「依赖层」): manifests, packages in `node_modules` (npm's flat layout and
//! pnpm's links into `.pnpm`), `package-lock.json` and declaration files,
//! written under a worktree root. Only synthetic content.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use lectern_services::code_intel::{DepOrigin, DepRoot, Ecosystem};
use lectern_services::project::BlobId;

use crate::deps::{DepMap, PathTable, Stat, WalkFile};
use crate::extract::ExtractStatus;

/// The path table of package `p` (`pkg-<p>`) with `files` indexed files,
/// paths in the package as long as [`ts_project`]'s (about 22 bytes, as
/// `types/m3/part_123.d.ts`), distinct OIDs.
pub(crate) fn npm_table(p: usize, files: usize) -> PathTable {
    let name = format!("pkg-{p:03}");
    let dir: Arc<str> = Arc::from(format!("node_modules/{name}"));
    let done = ExtractStatus::Done.code().unwrap_or(0);
    let files = (0..files)
        .map(|f| {
            let mut oid = [0u8; 20];
            oid[..8].copy_from_slice(&(p as u64).to_be_bytes());
            oid[8..16].copy_from_slice(&(f as u64).to_be_bytes());
            WalkFile {
                path: Arc::from(format!("{dir}/types/m{}/part_{f}.d.ts", f % 7)),
                oid: BlobId::from_bytes(&oid),
                stat: Stat::default(),
                status: done,
            }
        })
        .collect();
    PathTable {
        unit: None,
        origin: Arc::new(DepOrigin {
            ecosystem: Ecosystem::Npm,
            package: Arc::from(name.as_str()),
            version: Some(Arc::from("1.0.0")),
            root: DepRoot::Worktree { dir: Arc::from("") },
        }),
        identity: Arc::from(format!("npm:sha512-{name}").into_bytes()),
        manifest: Stat::default(),
        dir,
        recorded: 0,
        types: None,
        files,
    }
}

/// Synthetic path tables of `packages` packages of `files` files each, for
/// `code_intel/deps_map_bytes`: built before the measurement, the map
/// built from them measured.
pub struct DepTables(Vec<Arc<PathTable>>);

/// A dependency map built from [`DepTables`].
pub struct DepMapProbe(DepMap);

impl DepTables {
    pub fn new(packages: usize, files: usize) -> DepTables {
        DepTables(
            (0..packages)
                .map(|p| Arc::new(npm_table(p, files)))
                .collect(),
        )
    }

    pub fn map(&self) -> DepMapProbe {
        DepMapProbe(DepMap::build(1, &self.0))
    }
}

impl DepMapProbe {
    /// The files in the map.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.len() == 0
    }
}

/// Writes an npm tree under a worktree root.
#[derive(Clone)]
pub struct NpmTree {
    root: PathBuf,
}

impl NpmTree {
    pub fn new(root: &Path) -> NpmTree {
        NpmTree {
            root: root.to_path_buf(),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Writes `bytes` at `rel` (relative to the root), making its
    /// directories.
    pub fn write(&self, rel: &str, bytes: &[u8]) {
        let path = self.root.join(rel);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).expect("directories");
        }
        std::fs::write(&path, bytes).expect("a file");
    }

    pub fn remove(&self, rel: &str) {
        let path = self.root.join(rel);
        if path.is_dir() && !path.is_symlink() {
            std::fs::remove_dir_all(&path).expect("removed");
        } else {
            std::fs::remove_file(&path).expect("removed");
        }
    }

    /// A symbolic link at `rel` whose target is `target`, as written.
    pub fn link(&self, rel: &str, target: &str) {
        let path = self.root.join(rel);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).expect("directories");
        }
        std::os::unix::fs::symlink(target, &path).expect("a link");
    }

    /// The manifest of `dir` ("" for the root) with `deps` in its
    /// `dependencies`, and `extra` fields (JSON members, without braces).
    pub fn manifest(&self, dir: &str, deps: &[&str], extra: &str) {
        let mut json = String::from("{\n  \"name\": \"app\",\n  \"dependencies\": {");
        for (i, d) in deps.iter().enumerate() {
            let sep = if i == 0 { "" } else { "," };
            let _ = write!(json, "{sep}\n    \"{d}\": \"^1.0.0\"");
        }
        json.push_str("\n  }");
        if !extra.is_empty() {
            let _ = write!(json, ",\n  {extra}");
        }
        json.push_str("\n}\n");
        self.write(&join(dir, "package.json"), json.as_bytes());
    }

    /// A package `name` at `dir` (its directory relative to the root, such
    /// as `node_modules/react`): a `package.json` with `name`, `version`
    /// and `extra` fields, and `files` (relative to the package).
    pub fn package(
        &self,
        dir: &str,
        name: &str,
        version: &str,
        extra: &str,
        files: &[(&str, &str)],
    ) {
        let mut json = format!("{{\n  \"name\": \"{name}\",\n  \"version\": \"{version}\"");
        if !extra.is_empty() {
            let _ = write!(json, ",\n  {extra}");
        }
        json.push_str("\n}\n");
        self.write(&join(dir, "package.json"), json.as_bytes());
        for (rel, text) in files {
            self.write(&join(dir, rel), text.as_bytes());
        }
    }

    /// `package-lock.json` in `dir` with `packages` entries (key, version,
    /// integrity), and `padding` more bytes of other entries before them.
    pub fn lockfile(&self, dir: &str, entries: &[(&str, &str, &str)], padding: usize) {
        let mut json = String::from(
            "{\n  \"name\": \"app\",\n  \"lockfileVersion\": 3,\n  \"packages\": {\n    \"\": { \"name\": \"app\" }",
        );
        let mut i = 0;
        while json.len() < padding {
            let _ = write!(
                json,
                ",\n    \"node_modules/padding-{i}\": {{ \"version\": \"1.0.{i}\", \"resolved\": \"https://registry.example/padding-{i}.tgz\", \"integrity\": \"sha512-{}\", \"dependencies\": {{ \"x\": \"1\" }} }}",
                "p".repeat(64)
            );
            i += 1;
        }
        for (key, version, integrity) in entries {
            let _ = write!(
                json,
                ",\n    \"{key}\": {{ \"version\": \"{version}\", \"integrity\": \"{integrity}\", \"license\": \"MIT\" }}"
            );
        }
        json.push_str("\n  }\n}\n");
        self.write(&join(dir, "package-lock.json"), json.as_bytes());
    }
}

/// `dir` joined with `rel` ("" is the root).
pub fn join(dir: &str, rel: &str) -> String {
    if dir.is_empty() {
        rel.to_owned()
    } else {
        format!("{dir}/{rel}")
    }
}

/// A declaration file of about `len` bytes declaring `name_<n>` functions,
/// importing `imports` (relative specifiers, as written).
pub fn declarations(name: &str, imports: &[&str], len: usize) -> String {
    let mut out = String::new();
    for (i, s) in imports.iter().enumerate() {
        let _ = writeln!(out, "export * from \"{s}\";");
        let _ = writeln!(out, "import {{ value_{i} }} from \"{s}\";");
    }
    let mut n = 0;
    while out.len() < len.max(1) {
        let _ = writeln!(
            out,
            "export declare function {name}_{n}(a: number, b: string): Promise<{name}_{n}_Result>;\nexport interface {name}_{n}_Result {{ value: number; label: string; }}"
        );
        n += 1;
    }
    out
}

/// A TS project shaped like ADR 0033's research (`code_intel/deps_*`):
/// `direct` direct dependencies in the root manifest, each a package of
/// `files` declaration files of about `file_len` bytes chained by relative
/// `export * from`, entries by `types`; a `package-lock.json` of about
/// `lockfile_len` bytes with their integrity. Returns the package names.
pub fn ts_project(
    tree: &NpmTree,
    direct: usize,
    files: usize,
    file_len: usize,
    lockfile_len: usize,
) -> Vec<String> {
    let names: Vec<String> = (0..direct).map(|i| format!("dep-{i:02}")).collect();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    tree.manifest("", &refs, "");
    let mut entries = Vec::new();
    for (p, name) in names.iter().enumerate() {
        let dir = format!("node_modules/{name}");
        let mut decls = Vec::new();
        for f in 0..files {
            // Relative to each file: `types/part_0.d.ts` imports `./part_1`.
            let next = format!("./part_{}", f + 1);
            let imports: Vec<&str> = if f + 1 < files {
                vec![&next]
            } else {
                Vec::new()
            };
            let text = declarations(&format!("p{p}_f{f}"), &imports, file_len);
            decls.push((format!("types/part_{f}.d.ts"), text));
        }
        let files: Vec<(&str, &str)> = decls
            .iter()
            .map(|(r, t)| (r.as_str(), t.as_str()))
            .collect();
        tree.package(
            &dir,
            name,
            "1.0.0",
            "\"types\": \"types/part_0.d.ts\"",
            &files,
        );
        entries.push((dir, format!("sha512-{name}-integrity")));
    }
    let entries: Vec<(&str, &str, &str)> = entries
        .iter()
        .map(|(k, i)| (k.as_str(), "1.0.0", i.as_str()))
        .collect();
    tree.lockfile("", &entries, lockfile_len);
    names
}
