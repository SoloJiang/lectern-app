//! The language registry: the first-release languages, their grammars and
//! queries, and the current language table with compatibility groups
//! (docs/design/code_intel.md「语言与抽取键」).

use std::ffi::CString;
use std::fmt;
use std::sync::{Arc, OnceLock};

use tree_sitter_tags::TagsConfiguration;

use crate::grammar::{GrammarLease, GrammarSource, LoadedGrammar};
use crate::util::{fnv64, log};
use crate::{DefKind, LangName};

/// Bumped when the extractor's own rules change (visibility, parameter
/// counts, scopes, markers): every query version changes with it, so old
/// results stop matching.
pub const EXTRACTOR_VERSION: u32 = 1;

/// The tree-sitter runtime and tags crate versions, pinned in the root
/// `Cargo.toml` (ADR 0030; `lang_grammar_versions_match_lock`). Part of
/// every query version: the tags crate decides which matches become tags.
pub(crate) const TREE_SITTER_VERSION: &str = "0.27.1";
pub(crate) const TREE_SITTER_TAGS_VERSION: &str = "0.27.1";

/// How the extractor reads a language's tags: visibility, parameter counts
/// and reference sites follow per-language rules (`rules`, `site`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Dialect {
    TypeScript,
    Tsx,
    JavaScript,
    Rust,
    Java,
    Python,
    Go,
    /// A language pack: no Lectern rules.
    Pack,
}

/// One core language: linked into the binary (ADR 0018's core set).
pub(crate) struct CoreSpec {
    pub name: &'static str,
    pub extensions: &'static [&'static str],
    /// The grammar crate and its version, as locked in Cargo.lock
    /// (`lang_grammar_versions_match_lock`).
    pub grammar_crate: &'static str,
    pub grammar_crate_version: &'static str,
    pub compat_group: Option<&'static str>,
    pub dialect: Dialect,
    pub language: fn() -> tree_sitter::Language,
    pub tags_query: &'static str,
    pub locals_query: &'static str,
}

macro_rules! query {
    ($($file:literal),+) => {
        concat!($(include_str!(concat!("../queries/", $file))),+)
    };
}

/// The first-release languages (docs/design/code_intel.md「未决问题」1,
/// default (a)). To change the set, edit this list and the grammar crates in
/// the root and crate `Cargo.toml`; nothing else names a core language.
pub(crate) static CORE_LANGUAGES: &[CoreSpec] = &[
    CoreSpec {
        name: "ts",
        extensions: &["ts", "mts", "cts"],
        grammar_crate: "tree-sitter-typescript",
        grammar_crate_version: "0.23.2",
        compat_group: Some("js"),
        dialect: Dialect::TypeScript,
        language: || tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        tags_query: query!("ecmascript/tags.scm", "ts/tags.scm"),
        locals_query: query!("ecmascript/locals.scm", "ts/locals.scm"),
    },
    CoreSpec {
        name: "tsx",
        extensions: &["tsx"],
        grammar_crate: "tree-sitter-typescript",
        grammar_crate_version: "0.23.2",
        compat_group: Some("js"),
        dialect: Dialect::Tsx,
        language: || tree_sitter_typescript::LANGUAGE_TSX.into(),
        tags_query: query!("ecmascript/tags.scm", "ts/tags.scm", "jsx/tags.scm"),
        locals_query: query!("ecmascript/locals.scm", "ts/locals.scm"),
    },
    CoreSpec {
        name: "js",
        extensions: &["js", "mjs", "cjs", "jsx"],
        grammar_crate: "tree-sitter-javascript",
        grammar_crate_version: "0.25.0",
        compat_group: Some("js"),
        dialect: Dialect::JavaScript,
        language: || tree_sitter_javascript::LANGUAGE.into(),
        tags_query: query!("ecmascript/tags.scm", "js/tags.scm", "jsx/tags.scm"),
        locals_query: query!("ecmascript/locals.scm"),
    },
    CoreSpec {
        name: "rust",
        extensions: &["rs"],
        grammar_crate: "tree-sitter-rust",
        grammar_crate_version: "0.24.2",
        compat_group: None,
        dialect: Dialect::Rust,
        language: || tree_sitter_rust::LANGUAGE.into(),
        tags_query: query!("rust/tags.scm"),
        locals_query: query!("rust/locals.scm"),
    },
    CoreSpec {
        name: "java",
        extensions: &["java"],
        grammar_crate: "tree-sitter-java",
        grammar_crate_version: "0.23.5",
        compat_group: Some("jvm"),
        dialect: Dialect::Java,
        language: || tree_sitter_java::LANGUAGE.into(),
        tags_query: query!("java/tags.scm"),
        locals_query: query!("java/locals.scm"),
    },
    CoreSpec {
        name: "python",
        extensions: &["py", "pyi"],
        grammar_crate: "tree-sitter-python",
        grammar_crate_version: "0.25.0",
        compat_group: None,
        dialect: Dialect::Python,
        language: || tree_sitter_python::LANGUAGE.into(),
        tags_query: query!("python/tags.scm"),
        locals_query: query!("python/locals.scm"),
    },
    CoreSpec {
        name: "go",
        extensions: &["go"],
        grammar_crate: "tree-sitter-go",
        grammar_crate_version: "0.25.0",
        compat_group: None,
        dialect: Dialect::Go,
        language: || tree_sitter_go::LANGUAGE.into(),
        tags_query: query!("go/tags.scm"),
        locals_query: query!("go/locals.scm"),
    },
];

/// The grammar version of a core language: a hash of "crate@version".
pub(crate) fn core_grammar_version(spec: &CoreSpec) -> u64 {
    fnv64(&[
        spec.grammar_crate.as_bytes(),
        b"@",
        spec.grammar_crate_version.as_bytes(),
    ])
}

/// The query version: the tags and locals text, the tree-sitter and tags
/// crate versions and [`EXTRACTOR_VERSION`].
pub(crate) fn query_version(tags: &str, locals: &str) -> u64 {
    query_version_with(EXTRACTOR_VERSION, tags, locals)
}

pub(crate) fn query_version_with(extractor: u32, tags: &str, locals: &str) -> u64 {
    fnv64(&[
        &extractor.to_le_bytes(),
        b"tree-sitter@",
        TREE_SITTER_VERSION.as_bytes(),
        b"tree-sitter-tags@",
        TREE_SITTER_TAGS_VERSION.as_bytes(),
        b"\0",
        tags.as_bytes(),
        b"\0",
        locals.as_bytes(),
    ])
}

/// One language of the current table: a core language or an installed pack.
/// `Clone` shares it; every clone holds the grammar's lease.
#[derive(Clone)]
pub struct Lang(Arc<LangInner>);

struct LangInner {
    name: LangName,
    dialect: Dialect,
    grammar_version: u64,
    query_version: u64,
    compat_group: Option<Arc<str>>,
    language: tree_sitter::Language,
    tags_query: Arc<str>,
    locals_query: Arc<str>,
    lease: GrammarLease,
    /// Compiled on first use, on the calling (background) thread; `None`
    /// when compiling failed, which disables the language for this run.
    compiled: OnceLock<Option<Arc<Compiled>>>,
}

impl Lang {
    fn core(spec: &CoreSpec) -> Lang {
        Lang(Arc::new(LangInner {
            name: crate::lang_name(spec.name),
            dialect: spec.dialect,
            grammar_version: core_grammar_version(spec),
            query_version: query_version(spec.tags_query, spec.locals_query),
            compat_group: spec.compat_group.map(Arc::from),
            language: (spec.language)(),
            tags_query: Arc::from(spec.tags_query),
            locals_query: Arc::from(spec.locals_query),
            lease: GrammarLease::none(),
            compiled: OnceLock::new(),
        }))
    }

    fn pack(name: LangName, grammar: LoadedGrammar) -> Lang {
        let locals = grammar.locals_query.unwrap_or_else(|| Arc::from(""));
        let mut version = [0; 8];
        version.copy_from_slice(&grammar.artifact_sha256[..8]);
        Lang(Arc::new(LangInner {
            query_version: query_version(&grammar.tags_query, &locals),
            name,
            dialect: Dialect::Pack,
            grammar_version: u64::from_be_bytes(version),
            compat_group: grammar.compat_group,
            language: grammar.language,
            tags_query: grammar.tags_query,
            locals_query: locals,
            lease: grammar.lease,
            compiled: OnceLock::new(),
        }))
    }

    pub fn name(&self) -> &LangName {
        &self.0.name
    }

    /// Core languages: a hash of the grammar crate and version; packs: the
    /// first 8 bytes of `artifact_sha256`.
    pub fn grammar_version(&self) -> u64 {
        self.0.grammar_version
    }

    /// A hash of the tags and locals queries, the tags crate version and
    /// [`EXTRACTOR_VERSION`].
    pub fn query_version(&self) -> u64 {
        self.0.query_version
    }

    pub fn compat_group(&self) -> Option<&str> {
        self.0.compat_group.as_deref()
    }

    pub fn lease(&self) -> &GrammarLease {
        &self.0.lease
    }

    /// Compiles the queries now if they are not yet. Blocking; background
    /// threads only. False when the grammar or a query does not compile: the
    /// language is then unavailable for this run (one line on stderr).
    pub fn prepare(&self) -> bool {
        self.compiled().is_some()
    }

    pub(crate) fn dialect(&self) -> Dialect {
        self.0.dialect
    }

    pub(crate) fn language(&self) -> &tree_sitter::Language {
        &self.0.language
    }

    pub(crate) fn compiled(&self) -> Option<&Arc<Compiled>> {
        self.0
            .compiled
            .get_or_init(|| match Compiled::new(&self.0) {
                Ok(compiled) => Some(Arc::new(compiled)),
                Err(error) => {
                    log(format_args!(
                        "the {} grammar or its queries do not compile, the language is disabled: {error}",
                        self.0.name.0
                    ));
                    None
                }
            })
            .as_ref()
    }
}

impl fmt::Debug for Lang {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Lang")
            .field("name", &self.0.name)
            .field(
                "grammar_version",
                &format_args!("{:016x}", self.0.grammar_version),
            )
            .field(
                "query_version",
                &format_args!("{:016x}", self.0.query_version),
            )
            .field("compat_group", &self.0.compat_group)
            .finish_non_exhaustive()
    }
}

/// What a tags capture means to the extractor.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum TagClass {
    Def(DefKind),
    Ref,
    Marker(Marker),
}

/// `reference.lectern.*` captures: read by the extractor, never stored.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Marker {
    /// JS / TS: an export statement; definitions inside it are exported.
    Export,
    /// JS / TS: a name exported by `export { name }` or `export default name`.
    ExportName,
    /// JS / TS: an import statement; with `Export`, marks an ES module.
    Module,
    /// Rust: a visibility modifier at the start of an item.
    Pub,
    /// Rust: an inherent `impl` block and the type it belongs to.
    Impl,
    /// Rust: a trait `impl` block and the type it belongs to.
    ImplTrait,
    /// Java: a `private` modifier at the start of a declaration.
    Private,
    /// Go: a method declaration and its receiver type.
    Owner,
}

impl Marker {
    fn from_kind(kind: &str) -> Option<Marker> {
        Some(match kind {
            "export" => Marker::Export,
            "export-name" => Marker::ExportName,
            "module" => Marker::Module,
            "pub" => Marker::Pub,
            "impl" => Marker::Impl,
            "impl-trait" => Marker::ImplTrait,
            "private" => Marker::Private,
            "owner" => Marker::Owner,
            _ => return None,
        })
    }
}

/// What a capture of the combined `locals + tags` query is, for
/// `ParsedFile`, which runs the query itself.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Capture {
    Name,
    Doc,
    Ignore,
    LocalScope,
    LocalDefinition,
    LocalReference,
    Tag(TagClass),
    Other,
}

/// A language's compiled queries.
pub(crate) struct Compiled {
    /// `locals + tags`, as tree-sitter-tags builds it. `ParsedFile` runs
    /// `tags.query` itself on its own tree.
    pub tags: TagsConfiguration,
    /// By tags syntax type ID: `[as reference, as definition]`.
    classes: Vec<[TagClass; 2]>,
    /// By capture index of `tags.query`.
    pub captures: Vec<Capture>,
    /// Patterns below this index come from the locals query.
    pub locals_patterns: usize,
}

impl Compiled {
    fn new(lang: &LangInner) -> Result<Compiled, String> {
        let tags =
            TagsConfiguration::new(lang.language.clone(), &lang.tags_query, &lang.locals_query)
                .map_err(|e| e.to_string())?;
        // tree-sitter-tags numbers the kinds after `definition.` and
        // `reference.` in capture order, one ID per distinct kind; the same
        // walk gives the same IDs, checked against `syntax_type_name`.
        let mut kinds: Vec<&str> = Vec::new();
        for name in tags.query.capture_names() {
            let Some(kind) = name
                .strip_prefix("definition.")
                .or_else(|| name.strip_prefix("reference."))
            else {
                continue;
            };
            if CString::new(kind).is_ok() && !kinds.contains(&kind) {
                kinds.push(kind);
            }
        }
        let mut classes = Vec::with_capacity(kinds.len());
        for (id, kind) in kinds.iter().enumerate() {
            let id = u32::try_from(id).map_err(|e| e.to_string())?;
            if tags.syntax_type_name(id) != *kind {
                return Err(format!("syntax type {id} is not {kind:?}"));
            }
            let reference = match kind.strip_prefix("lectern.") {
                Some(marker) => TagClass::Marker(
                    Marker::from_kind(marker)
                        .ok_or_else(|| format!("unknown marker reference.{kind}"))?,
                ),
                None => TagClass::Ref,
            };
            classes.push([reference, TagClass::Def(crate::def_kind_from_tag(kind))]);
        }
        let captures = tags
            .query
            .capture_names()
            .iter()
            .map(|name| match *name {
                "name" => Capture::Name,
                "doc" => Capture::Doc,
                "ignore" => Capture::Ignore,
                "local.scope" => Capture::LocalScope,
                "local.definition" => Capture::LocalDefinition,
                "local.reference" => Capture::LocalReference,
                _ => {
                    let (kind, is_definition) = match name.strip_prefix("definition.") {
                        Some(kind) => (kind, true),
                        None => match name.strip_prefix("reference.") {
                            Some(kind) => (kind, false),
                            None => return Capture::Other,
                        },
                    };
                    match kinds.iter().position(|k| *k == kind) {
                        Some(id) => Capture::Tag(classes[id][usize::from(is_definition)]),
                        None => Capture::Other,
                    }
                }
            })
            .collect();
        let locals_patterns = (0..tags.query.pattern_count())
            .filter(|&i| tags.query.start_byte_for_pattern(i) < lang.locals_query.len())
            .count();
        Ok(Compiled {
            tags,
            classes,
            captures,
            locals_patterns,
        })
    }

    pub(crate) fn class(&self, syntax_type_id: u32, is_definition: bool) -> TagClass {
        self.classes
            .get(syntax_type_id as usize)
            .map_or(TagClass::Ref, |c| c[usize::from(is_definition)])
    }
}

/// The current language table: the core languages and the installed packs,
/// each with its compatibility group. Replaced as a whole when packs change
/// (docs/design/code_intel.md「语言包的变化」).
#[derive(Clone)]
pub struct LanguageTable {
    langs: Arc<[Lang]>,
}

impl LanguageTable {
    /// The core languages only. Compiles nothing.
    pub fn core() -> LanguageTable {
        LanguageTable {
            langs: CORE_LANGUAGES.iter().map(Lang::core).collect(),
        }
    }

    pub fn get(&self, name: &str) -> Option<&Lang> {
        self.langs.iter().find(|l| &*l.name().0 == name)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Lang> + '_ {
        self.langs.iter()
    }

    /// The language of a path: a core extension first, then the extension a
    /// pack claims, if that pack is in this table.
    pub fn for_path(&self, path: &str, packs: &dyn GrammarSource) -> Option<&Lang> {
        let name = language_for_path(path, packs)?;
        self.get(&name.0)
    }

    /// This table with a pack added or replaced. A pack named like a core
    /// language is ignored: core languages win.
    pub fn with_pack(&self, name: LangName, grammar: LoadedGrammar) -> LanguageTable {
        if core_spec(&name.0).is_some() {
            return self.clone();
        }
        let mut langs: Vec<Lang> = self
            .langs
            .iter()
            .filter(|l| l.name() != &name)
            .cloned()
            .collect();
        langs.push(Lang::pack(name, grammar));
        LanguageTable {
            langs: langs.into(),
        }
    }

    /// This table without a pack. Core languages stay.
    pub fn without(&self, name: &str) -> LanguageTable {
        if core_spec(name).is_some() {
            return self.clone();
        }
        LanguageTable {
            langs: self
                .langs
                .iter()
                .filter(|l| &*l.name().0 != name)
                .cloned()
                .collect(),
        }
    }
}

impl fmt::Debug for LanguageTable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.langs.iter()).finish()
    }
}

fn core_spec(name: &str) -> Option<&'static CoreSpec> {
    CORE_LANGUAGES.iter().find(|s| s.name == name)
}

/// The extension of a path's file name: what follows its last dot, unless
/// the dot starts the name (`.gitignore` has none).
fn extension(path: &str) -> Option<&str> {
    let file = path.rsplit('/').next().unwrap_or(path);
    match file.rfind('.') {
        Some(0) | None => None,
        Some(i) => Some(&file[i + 1..]),
    }
}

fn core_for_extension(ext: &str) -> Option<&'static CoreSpec> {
    let lookup = |e: &str| CORE_LANGUAGES.iter().find(|s| s.extensions.contains(&e));
    lookup(ext).or_else(|| {
        let lower = ext.to_ascii_lowercase();
        (lower != ext).then(|| lookup(&lower)).flatten()
    })
}

/// The language of a path, from its extension alone: core languages first,
/// then the packs' claims.
pub fn language_for_path(path: &str, packs: &dyn GrammarSource) -> Option<LangName> {
    let ext = extension(path)?;
    match core_for_extension(ext) {
        Some(spec) => Some(crate::lang_name(spec.name)),
        None => packs.language_for_extension(ext),
    }
}

/// Languages in the same compatibility group may reference each other; a
/// language without a group is compatible with itself only. Groups come from
/// the current table, since packs register theirs.
pub fn compatible(langs: &LanguageTable, a: &LangName, b: &LangName) -> bool {
    if a == b {
        return true;
    }
    match (langs.get(&a.0), langs.get(&b.0)) {
        (Some(a), Some(b)) => a.compat_group().is_some() && a.compat_group() == b.compat_group(),
        _ => false,
    }
}
