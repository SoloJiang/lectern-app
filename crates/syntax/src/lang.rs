//! The highlighter's language table: the core languages of ADR 0018, their
//! grammars and highlight queries (docs/design/file_reader.md「语言与区间」).

use std::fmt;
use std::sync::{Arc, OnceLock};

use tree_sitter::Query;

use crate::spans::TokenKind;
use crate::util::{fnv64, log};

/// Bumped when the way query captures become tokens changes (the capture
/// name mapping, the precedence rules, line splitting): every query version
/// changes with it, so cached spans stop matching.
pub const SPANS_VERSION: u32 = 1;

/// The tree-sitter runtime, pinned in the root `Cargo.toml` (ADR 0030) and
/// checked against Cargo.lock by `lang_grammar_versions_match_lock`. Part of
/// every query version.
pub(crate) const TREE_SITTER_VERSION: &str = "0.27.1";

/// The rule family for safe window starts (ADR 0034; `window::safe_start`).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Rules {
    /// A line at column 0 that is not a closing bracket or a comment
    /// continuation.
    Code,
    /// The start of an object member, `,"key":`.
    Json,
    /// After a blank line.
    Markup,
}

/// Which pattern colours a node that several patterns capture. Upstream
/// queries are written for one or the other; each query file's header says
/// which one Lectern applies to it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Precedence {
    /// The first matching pattern in the query wins (tree-sitter-highlight's
    /// rule: specific patterns first, `(identifier) @variable` last).
    First,
    /// The last matching pattern wins (general patterns first).
    Last,
}

/// One core language, linked into the binary.
pub(crate) struct CoreSpec {
    pub name: &'static str,
    pub extensions: &'static [&'static str],
    /// The grammar crate and its version, as locked in Cargo.lock.
    pub grammar_crate: &'static str,
    pub grammar_crate_version: &'static str,
    pub language: fn() -> tree_sitter::Language,
    pub query: &'static str,
    pub rules: Rules,
    pub precedence: Precedence,
}

macro_rules! query {
    ($($file:literal),+) => {
        concat!($(include_str!(concat!("../queries/", $file, "/highlights.scm"))),+)
    };
}

/// ADR 0018's core highlighting languages: `code_intel`'s seven, with the
/// same names and extensions (`health` keys by name; a cross-check against
/// `code_intel`'s table comes with its `health` registration), and ADR
/// 0041's four.
pub(crate) static CORE_LANGUAGES: &[CoreSpec] = &[
    CoreSpec {
        name: "ts",
        extensions: &["ts", "mts", "cts"],
        grammar_crate: "tree-sitter-typescript",
        grammar_crate_version: "0.23.2",
        language: || tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        query: query!("ecmascript", "ts"),
        rules: Rules::Code,
        precedence: Precedence::Last,
    },
    CoreSpec {
        name: "tsx",
        extensions: &["tsx"],
        grammar_crate: "tree-sitter-typescript",
        grammar_crate_version: "0.23.2",
        language: || tree_sitter_typescript::LANGUAGE_TSX.into(),
        query: query!("ecmascript", "jsx", "ts"),
        rules: Rules::Code,
        precedence: Precedence::Last,
    },
    CoreSpec {
        name: "js",
        extensions: &["js", "mjs", "cjs", "jsx"],
        grammar_crate: "tree-sitter-javascript",
        grammar_crate_version: "0.25.0",
        language: || tree_sitter_javascript::LANGUAGE.into(),
        query: query!("ecmascript", "js", "jsx"),
        rules: Rules::Code,
        precedence: Precedence::Last,
    },
    CoreSpec {
        name: "rust",
        extensions: &["rs"],
        grammar_crate: "tree-sitter-rust",
        grammar_crate_version: "0.24.2",
        language: || tree_sitter_rust::LANGUAGE.into(),
        query: query!("rust"),
        rules: Rules::Code,
        precedence: Precedence::First,
    },
    CoreSpec {
        name: "java",
        extensions: &["java"],
        grammar_crate: "tree-sitter-java",
        grammar_crate_version: "0.23.5",
        language: || tree_sitter_java::LANGUAGE.into(),
        query: query!("java"),
        rules: Rules::Code,
        precedence: Precedence::Last,
    },
    CoreSpec {
        name: "python",
        extensions: &["py", "pyi"],
        grammar_crate: "tree-sitter-python",
        grammar_crate_version: "0.25.0",
        language: || tree_sitter_python::LANGUAGE.into(),
        query: query!("python"),
        rules: Rules::Code,
        precedence: Precedence::Last,
    },
    CoreSpec {
        name: "go",
        extensions: &["go"],
        grammar_crate: "tree-sitter-go",
        grammar_crate_version: "0.25.0",
        language: || tree_sitter_go::LANGUAGE.into(),
        query: query!("go"),
        rules: Rules::Code,
        precedence: Precedence::First,
    },
    CoreSpec {
        name: "json",
        extensions: &["json", "jsonc", "jsonl"],
        grammar_crate: "tree-sitter-json",
        grammar_crate_version: "0.24.8",
        language: || tree_sitter_json::LANGUAGE.into(),
        query: query!("json"),
        rules: Rules::Json,
        precedence: Precedence::First,
    },
    CoreSpec {
        name: "yaml",
        extensions: &["yaml", "yml"],
        grammar_crate: "tree-sitter-yaml",
        grammar_crate_version: "0.7.2",
        language: || tree_sitter_yaml::LANGUAGE.into(),
        query: query!("yaml"),
        rules: Rules::Code,
        precedence: Precedence::Last,
    },
    CoreSpec {
        name: "toml",
        extensions: &["toml"],
        grammar_crate: "tree-sitter-toml-ng",
        grammar_crate_version: "0.7.0",
        language: || tree_sitter_toml_ng::LANGUAGE.into(),
        query: query!("toml"),
        rules: Rules::Code,
        precedence: Precedence::Last,
    },
    CoreSpec {
        name: "markdown",
        extensions: &["md", "markdown"],
        grammar_crate: "tree-sitter-md",
        grammar_crate_version: "0.5.3",
        language: || tree_sitter_md::LANGUAGE.into(),
        query: query!("markdown"),
        rules: Rules::Markup,
        precedence: Precedence::Last,
    },
];

/// Whole file names whose extension does not name their language.
pub(crate) static FILE_NAMES: &[(&str, &str)] = &[
    ("Cargo.lock", "toml"),
    ("poetry.lock", "toml"),
    ("uv.lock", "toml"),
    ("Pipfile", "toml"),
    ("package-lock.json", "json"),
    (".babelrc", "json"),
    (".eslintrc", "json"),
];

/// The grammar version of a core language: the same rule as code_intel's
/// (「语言与抽取键」), the FNV-1a of "crate@version".
pub(crate) fn core_grammar_version(spec: &CoreSpec) -> u64 {
    fnv64(&[
        spec.grammar_crate.as_bytes(),
        b"@",
        spec.grammar_crate_version.as_bytes(),
    ])
}

/// The query version: the highlight query's text, the tree-sitter version
/// and [`SPANS_VERSION`].
pub(crate) fn query_version(query: &str) -> u64 {
    fnv64(&[
        &SPANS_VERSION.to_le_bytes(),
        b"tree-sitter@",
        TREE_SITTER_VERSION.as_bytes(),
        b"\0",
        query.as_bytes(),
    ])
}

/// What one capture of a highlight query does.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Capture {
    /// Colours its node.
    Kind(TokenKind),
    /// Leaves its node uncoloured, over any capture around it
    /// (`@none`, `@embedded`: a template substitution inside a string).
    Plain,
    /// Not a highlight: names starting with `_` (used by predicates) and
    /// names Lectern does not know.
    Ignore,
}

/// The capture name's meaning: the full name first, then its first
/// component. None for a name Lectern does not know (`lang_captures_known`
/// checks the core queries have none).
pub(crate) fn classify(name: &str) -> Option<Capture> {
    use TokenKind as K;
    let kind = match name {
        "none" | "embedded" => return Some(Capture::Plain),
        "escape" | "string.escape" => K::Escape,
        "string.special.key" => K::Property,
        "variable.builtin" => K::Keyword,
        "constructor" => K::Type,
        "boolean" => K::Constant,
        "label" => K::Other,
        "text.title" => K::Heading,
        "text.literal" => K::String,
        "text.uri" | "text.reference" => K::Link,
        "text.emphasis" | "text.strong" => K::Emphasis,
        _ if name.starts_with('_') => return Some(Capture::Ignore),
        _ => match name.split('.').next().unwrap_or(name) {
            "keyword" => K::Keyword,
            "string" => K::String,
            "number" => K::Number,
            "comment" => K::Comment,
            "function" => K::Function,
            "type" => K::Type,
            "constant" => K::Constant,
            "property" => K::Property,
            "variable" => K::Variable,
            "operator" => K::Operator,
            "punctuation" => K::Punctuation,
            "tag" => K::Tag,
            "attribute" => K::Attribute,
            _ => return None,
        },
    };
    Some(Capture::Kind(kind))
}

/// A language's compiled highlight query.
pub(crate) struct Compiled {
    pub query: Query,
    /// By capture index.
    pub captures: Box<[Capture]>,
}

impl Compiled {
    fn new(spec: &CoreSpec, language: &tree_sitter::Language) -> Result<Compiled, String> {
        let query = Query::new(language, spec.query).map_err(|e| e.to_string())?;
        let captures = query
            .capture_names()
            .iter()
            .map(|name| classify(name).unwrap_or(Capture::Ignore))
            .collect();
        Ok(Compiled { query, captures })
    }
}

/// One language of the table.
pub struct Lang {
    spec: &'static CoreSpec,
    language: tree_sitter::Language,
    grammar_version: u64,
    query_version: u64,
    /// Compiled on first use, on the calling (background) thread; None when
    /// it does not compile, which shows the language as plain text for this
    /// run.
    compiled: OnceLock<Option<Compiled>>,
}

impl Lang {
    fn core(spec: &'static CoreSpec) -> Lang {
        Lang {
            spec,
            language: (spec.language)(),
            grammar_version: core_grammar_version(spec),
            query_version: query_version(spec.query),
            compiled: OnceLock::new(),
        }
    }

    /// The same short name as code_intel's `LangName` ("ts", "rust", …),
    /// plus "json", "yaml", "toml" and "markdown".
    pub fn name(&self) -> &str {
        self.spec.name
    }

    /// The FNV-1a of "grammar crate@version", the rule of code_intel's
    /// 「语言与抽取键」.
    pub fn grammar_version(&self) -> u64 {
        self.grammar_version
    }

    /// The FNV-1a of the highlight query's text, the tree-sitter version and
    /// [`SPANS_VERSION`].
    pub fn query_version(&self) -> u64 {
        self.query_version
    }

    /// The safe window start rules.
    pub fn rules(&self) -> Rules {
        self.spec.rules
    }

    /// Compiles the highlight query now if it is not yet: once, then it
    /// stays. Blocking; background threads only. False when it does not
    /// compile (one line on stderr): the language is shown as plain text for
    /// this run.
    pub fn prepare(&self) -> bool {
        self.compiled().is_some()
    }

    pub(crate) fn language(&self) -> &tree_sitter::Language {
        &self.language
    }

    pub(crate) fn precedence(&self) -> Precedence {
        self.spec.precedence
    }

    pub(crate) fn compiled(&self) -> Option<&Compiled> {
        self.compiled
            .get_or_init(|| match Compiled::new(self.spec, &self.language) {
                Ok(compiled) => Some(compiled),
                Err(error) => {
                    log(format_args!(
                        "the {} highlight query does not compile, the language is shown as plain text: {error}",
                        self.spec.name
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
            .field("name", &self.spec.name)
            .field(
                "grammar_version",
                &format_args!("{:016x}", self.grammar_version),
            )
            .field(
                "query_version",
                &format_args!("{:016x}", self.query_version),
            )
            .finish_non_exhaustive()
    }
}

/// The highlighter's language table. The core languages are linked in; the
/// table is built on first use and compiles nothing (no IO either).
pub struct Languages {
    langs: Box<[Lang]>,
}

impl Languages {
    /// The core languages. Built once per process; later calls share it.
    pub fn core() -> Arc<Languages> {
        static CORE: OnceLock<Arc<Languages>> = OnceLock::new();
        CORE.get_or_init(|| {
            Arc::new(Languages {
                langs: CORE_LANGUAGES.iter().map(Lang::core).collect(),
            })
        })
        .clone()
    }

    /// By extension (ASCII case-insensitive when the exact one is unknown)
    /// and a few whole file names (`Cargo.lock` is TOML). None: plain text.
    pub fn for_path(&self, path: &str) -> Option<&Lang> {
        let file = path.rsplit('/').next().unwrap_or(path);
        if let Some((_, name)) = FILE_NAMES.iter().find(|(f, _)| *f == file) {
            return self.get(name);
        }
        let ext = extension(file)?;
        let find = |e: &str| self.langs.iter().find(|l| l.spec.extensions.contains(&e));
        find(ext).or_else(|| {
            let lower = ext.to_ascii_lowercase();
            (lower != ext).then(|| find(&lower)).flatten()
        })
    }

    pub fn get(&self, name: &str) -> Option<&Lang> {
        self.langs.iter().find(|l| l.spec.name == name)
    }

    /// The languages in table order.
    pub fn iter(&self) -> impl Iterator<Item = &Lang> + '_ {
        self.langs.iter()
    }
}

impl fmt::Debug for Languages {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.langs.iter()).finish()
    }
}

/// The extension of a file name: what follows its last dot, unless the dot
/// starts the name (`.gitignore` has none). code_intel's rule.
fn extension(file: &str) -> Option<&str> {
    match file.rfind('.') {
        Some(0) | None => None,
        Some(i) => Some(&file[i + 1..]),
    }
}
