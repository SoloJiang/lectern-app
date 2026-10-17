//! Compiles `locales/<locale>/*.ftl` into a checked intermediate form.
//!
//! Shared by `build.rs`, which turns the result into Rust code (see
//! `codegen.rs`), and `tests/validation.rs`, which runs the checks against
//! broken resources. Every rule in docs/design/i18n.md「编译期校验」 is
//! enforced here; any violation is an [`Error`] naming the file, line and key.

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::fs;
use std::path::Path;

use fluent_syntax::ast;
use fluent_syntax::parser;
use fluent_syntax::unicode::unescape_unicode_to_string;

/// A supported interface language. `en` comes first: the `keys` module and
/// the table order are generated from it.
pub struct LocaleSpec {
    /// BCP 47 tag, also the directory name under `locales/`.
    pub tag: &'static str,
    /// Name of the generated static table.
    pub table: &'static str,
    /// CLDR cardinal plural categories of the language.
    pub plural_categories: &'static [&'static str],
}

pub static LOCALES: [LocaleSpec; 2] = [
    LocaleSpec {
        tag: "en",
        table: "EN",
        plural_categories: &["one", "other"],
    },
    LocaleSpec {
        tag: "zh-CN",
        table: "ZH_CN",
        plural_categories: &["other"],
    },
];

/// Most variables one message may use; `Args` holds this many without
/// allocating. Must equal `MAX_ARGS` in src/args.rs.
pub const MAX_VARS: usize = 8;

/// Every CLDR plural category name. An identifier variant key with one of
/// these names selects by plural category when the selector is a number.
const CATEGORIES: [&str; 6] = ["zero", "one", "two", "few", "many", "other"];

/// A rule violation. `file` is relative to the locales directory
/// (`zh-CN/review.ftl`); `line` is 1-based, 0 when the error concerns the whole
/// file or directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    pub file: String,
    pub line: usize,
    pub key: Option<String>,
    pub message: String,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.file)?;
        if self.line > 0 {
            write!(f, ":{}", self.line)?;
        }
        if let Some(key) = &self.key {
            write!(f, ": {key}")?;
        }
        write!(f, ": {}", self.message)
    }
}

/// How a variable is used, inferred from the resources.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VarType {
    /// Selected by plural category or number literal.
    Num,
    /// Selected by string literal.
    Str,
    /// Only interpolated: either kind of argument works.
    Any,
}

impl VarType {
    pub fn describe(self) -> &'static str {
        match self {
            VarType::Num => "number",
            VarType::Str => "string",
            VarType::Any => "number or string",
        }
    }

    fn merge(self, other: VarType) -> Option<VarType> {
        match (self, other) {
            (VarType::Any, t) | (t, VarType::Any) => Some(t),
            (a, b) if a == b => Some(a),
            _ => None,
        }
    }
}

/// A variant key after classification.
#[derive(Clone, Debug, PartialEq)]
pub enum VariantKey {
    /// A plural category (`[one]`, `*[other]`).
    Plural(&'static str),
    /// A number literal (`[0]`).
    Exact(f64),
    /// A string literal (`[file]`).
    Str(String),
}

/// A compiled pattern node. Message and term references are already expanded.
#[derive(Clone, Debug, PartialEq)]
pub enum Node {
    Text(String),
    /// Index into [`Entry::vars`].
    Var(usize),
    /// `arms` keeps every variant in source order (Fluent picks the first
    /// match); `default` is the index of the `*[...]` variant in `arms`.
    Select {
        var: usize,
        arms: Vec<(VariantKey, Vec<Node>)>,
        default: usize,
    },
}

/// One message value or attribute: one slot in the tables, one key constant.
#[derive(Clone, Debug)]
pub struct Entry {
    /// Message ID, e.g. `review-looks-good`.
    pub message: String,
    /// Attribute name, e.g. `tooltip`.
    pub attribute: Option<String>,
    /// Constant name in the file's `keys` module, e.g. `LOOKS_GOOD_TOOLTIP`.
    pub const_name: String,
    /// Variables in order of first use in en, with the type both languages
    /// agree on.
    pub vars: Vec<(String, VarType)>,
}

impl Entry {
    /// `review-looks-good` or `review-looks-good.tooltip`.
    pub fn name(&self) -> String {
        entry_name(&self.message, self.attribute.as_deref())
    }
}

/// The entries of one en file, which become one `keys` submodule.
#[derive(Clone, Debug)]
pub struct Module {
    /// File name without `.ftl`, e.g. `review`.
    pub file: String,
    /// Rust module name, e.g. `review`.
    pub ident: String,
    pub entries: std::ops::Range<usize>,
}

#[derive(Clone, Debug)]
pub struct Compiled {
    pub modules: Vec<Module>,
    pub entries: Vec<Entry>,
    /// One table per [`LOCALES`] item, each with one pattern per entry.
    pub tables: Vec<Vec<Vec<Node>>>,
}

/// An FTL file loaded into memory.
pub struct Source {
    /// Index into [`LOCALES`].
    pub locale: usize,
    /// File name, e.g. `review.ftl`.
    pub name: String,
    pub text: String,
}

impl Source {
    fn label(&self) -> String {
        format!("{}/{}", LOCALES[self.locale].tag, self.name)
    }

    fn stem(&self) -> &str {
        self.name.strip_suffix(".ftl").unwrap_or(&self.name)
    }

    /// 1-based line of `slice`, which must point into `self.text`; 0 otherwise.
    fn line_of(&self, slice: &str) -> usize {
        let start = self.text.as_ptr() as usize;
        let at = slice.as_ptr() as usize;
        if at < start || at > start + self.text.len() {
            return 0;
        }
        self.text.as_bytes()[..at - start]
            .iter()
            .filter(|&&b| b == b'\n')
            .count()
            + 1
    }
}

/// Loads `<root>/<locale>/*.ftl` for every locale and compiles them.
pub fn compile_dir(root: &Path) -> Result<Compiled, Vec<Error>> {
    let mut errors = Vec::new();
    let mut sources = Vec::new();
    match fs::read_dir(root) {
        Ok(dir) => {
            let mut unknown: Vec<String> = dir
                .filter_map(|e| e.ok())
                .filter(|e| e.path().is_dir())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|name| !LOCALES.iter().any(|l| l.tag == name))
                .collect();
            unknown.sort();
            for name in unknown {
                errors.push(whole_file(
                    name,
                    "unknown locale directory (supported: en, zh-CN)".to_owned(),
                ));
            }
        }
        Err(e) => {
            return Err(vec![whole_file(
                root.display().to_string(),
                format!("cannot read: {e}"),
            )]);
        }
    }
    for (locale, spec) in LOCALES.iter().enumerate() {
        let dir = root.join(spec.tag);
        let listing = match fs::read_dir(&dir) {
            Ok(listing) => listing,
            Err(_) => {
                errors.push(whole_file(
                    spec.tag.to_owned(),
                    "missing locale directory".to_owned(),
                ));
                continue;
            }
        };
        let mut names: Vec<String> = listing
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|name| name.ends_with(".ftl"))
            .collect();
        names.sort();
        for name in names {
            match fs::read_to_string(dir.join(&name)) {
                Ok(text) => sources.push(Source { locale, name, text }),
                Err(e) => errors.push(whole_file(
                    format!("{}/{name}", spec.tag),
                    format!("cannot read: {e}"),
                )),
            }
        }
    }
    if !errors.is_empty() {
        return Err(errors);
    }
    compile(&sources)
}

/// Checks and compiles in-memory sources. Files of one locale must be given in
/// file name order.
pub fn compile(sources: &[Source]) -> Result<Compiled, Vec<Error>> {
    let mut errors = Errors::default();

    // Syntax. Later checks would only repeat the parser's complaints, so stop here.
    let mut resources = Vec::with_capacity(sources.len());
    for source in sources {
        match parser::parse(source.text.as_str()) {
            Ok(resource) => resources.push(resource),
            Err((resource, parse_errors)) => {
                for e in parse_errors {
                    let line = source.text.as_bytes()[..e.pos.start.min(source.text.len())]
                        .iter()
                        .filter(|&&b| b == b'\n')
                        .count()
                        + 1;
                    errors.push(Error {
                        file: source.label(),
                        line,
                        key: None,
                        message: format!("syntax error: {}", e.kind),
                    });
                }
                resources.push(resource);
            }
        }
    }
    if !errors.list.is_empty() {
        return Err(errors.list);
    }

    let indexes: Vec<LocaleIndex<'_>> = (0..LOCALES.len())
        .map(|locale| LocaleIndex::build(locale, sources, &resources, &mut errors))
        .collect();
    check_structure(sources, &indexes, &mut errors);

    // Entries come from en, in file order and then source order.
    let en = &indexes[0];
    let mut modules = Vec::new();
    let mut entries = Vec::new();
    let mut slots: Vec<(String, Option<String>)> = Vec::new();
    for &src in &en.files {
        let source = &sources[src];
        let start = entries.len();
        let mut consts: HashMap<String, String> = HashMap::new();
        for message in en.messages_in(src) {
            let id = message.id.name;
            let rest = id
                .strip_prefix(source.stem())
                .and_then(|r| r.strip_prefix('-'))
                .unwrap_or(id);
            let base = rest.to_ascii_uppercase().replace('-', "_");
            let mut names = Vec::new();
            if message.value.is_some() {
                names.push((None, base.clone()));
            }
            for (i, attr) in message.attributes.iter().enumerate() {
                // Later duplicates are already errors.
                if message.attributes[..i]
                    .iter()
                    .any(|a| a.id.name == attr.id.name)
                {
                    continue;
                }
                names.push((
                    Some(attr.id.name),
                    format!(
                        "{base}_{}",
                        attr.id.name.to_ascii_uppercase().replace('-', "_")
                    ),
                ));
            }
            for (attr, const_name) in names {
                let name = entry_name(id, attr);
                if let Some(other) = consts.insert(const_name.clone(), name.clone()) {
                    errors.push(Error {
                        file: source.label(),
                        line: source.line_of(attr.map_or(id, |a| find_attr(message, a).id.name)),
                        key: Some(name),
                        message: format!("key constant {const_name} is already used by {other}"),
                    });
                }
                entries.push(Entry {
                    message: id.to_owned(),
                    attribute: attr.map(str::to_owned),
                    const_name,
                    vars: Vec::new(),
                });
                slots.push((id.to_owned(), attr.map(str::to_owned)));
            }
        }
        modules.push(Module {
            file: source.stem().to_owned(),
            ident: source.stem().replace('-', "_"),
            entries: start..entries.len(),
        });
    }

    // Lower every entry in every locale and infer variable types.
    let mut lowered: Vec<Vec<Option<LoweredEntry>>> = Vec::new();
    for (locale, index) in indexes.iter().enumerate() {
        let mut lowerer = Lowerer {
            locale,
            sources,
            index,
            errors: &mut errors,
            reached: std::collections::HashSet::new(),
        };
        let mut table = Vec::with_capacity(slots.len());
        for (id, attr) in &slots {
            let Some(&(src, message)) = index.messages.get(id.as_str()) else {
                table.push(None);
                continue;
            };
            let (pattern, anchor) = match attr {
                Some(attr) => match message.attributes.iter().find(|a| a.id.name == attr) {
                    Some(a) => (&a.value, a.id.name),
                    None => {
                        table.push(None);
                        continue;
                    }
                },
                None => match &message.value {
                    Some(value) => (value, message.id.name),
                    None => {
                        table.push(None);
                        continue;
                    }
                },
            };
            let name = entry_name(id, attr.as_deref());
            let nodes = lowerer.entry(src, &name, anchor, pattern);
            let vars = lowerer.vars(src, &name, anchor, &nodes);
            table.push(Some((nodes, vars)));
        }
        // Terms no message references are validated on their own.
        let mut terms: Vec<_> = index.terms.values().copied().collect();
        terms.sort_by_key(|(src, term)| (*src, term.id.name));
        for (src, term) in terms {
            lowerer.term(src, term);
        }
        lowered.push(table);
    }

    // Variables must agree across languages.
    for (slot, entry) in entries.iter_mut().enumerate() {
        let Some((_, en_vars)) = &lowered[0][slot] else {
            continue;
        };
        let mut vars = en_vars.clone();
        for locale in 1..LOCALES.len() {
            let Some((_, other)) = &lowered[locale][slot] else {
                continue;
            };
            let index = &indexes[locale];
            let (src, message) = index.messages[entry.message.as_str()];
            let anchor = entry
                .attribute
                .as_deref()
                .map_or(message.id.name, |a| find_attr(message, a).id.name);
            let source = &sources[src];
            let mut en_names: Vec<&str> = en_vars.iter().map(|(n, _)| n.as_str()).collect();
            let mut other_names: Vec<&str> = other.iter().map(|(n, _)| n.as_str()).collect();
            en_names.sort_unstable();
            other_names.sort_unstable();
            if en_names != other_names {
                errors.push(Error {
                    file: source.label(),
                    line: source.line_of(anchor),
                    key: Some(entry.name()),
                    message: format!(
                        "uses {} but en uses {}",
                        var_list(&other_names),
                        var_list(&en_names)
                    ),
                });
                continue;
            }
            for (name, ty) in vars.iter_mut() {
                let theirs = other
                    .iter()
                    .find(|(n, _)| n == name)
                    .map(|(_, t)| *t)
                    .unwrap_or(VarType::Any);
                match ty.merge(theirs) {
                    Some(merged) => *ty = merged,
                    None => errors.push(Error {
                        file: source.label(),
                        line: source.line_of(anchor),
                        key: Some(entry.name()),
                        message: format!(
                            "${name} is a {} here but a {} in en; one Args value cannot satisfy both",
                            theirs.describe(),
                            ty.describe()
                        ),
                    }),
                }
            }
        }
        if vars.len() > MAX_VARS {
            let (src, message) = en.messages[entry.message.as_str()];
            let anchor = entry
                .attribute
                .as_deref()
                .map_or(message.id.name, |a| find_attr(message, a).id.name);
            errors.push(Error {
                file: sources[src].label(),
                line: sources[src].line_of(anchor),
                key: Some(entry.name()),
                message: format!(
                    "uses {} variables; at most {MAX_VARS} are supported",
                    vars.len()
                ),
            });
        }
        entry.vars = vars;
    }

    if !errors.list.is_empty() {
        return Err(errors.list);
    }

    let tables = lowered
        .into_iter()
        .map(|table| {
            table
                .into_iter()
                .zip(&entries)
                .map(|(slot, entry)| {
                    let (nodes, _) = slot.expect("every slot is filled when there are no errors");
                    resolve_vars(nodes, &entry.vars)
                })
                .collect()
        })
        .collect();
    Ok(Compiled {
        modules,
        entries,
        tables,
    })
}

fn whole_file(file: String, message: String) -> Error {
    Error {
        file,
        line: 0,
        key: None,
        message,
    }
}

fn entry_name(message: &str, attribute: Option<&str>) -> String {
    match attribute {
        Some(attr) => format!("{message}.{attr}"),
        None => message.to_owned(),
    }
}

fn find_attr<'a>(message: &'a ast::Message<&'a str>, name: &str) -> &'a ast::Attribute<&'a str> {
    message
        .attributes
        .iter()
        .find(|a| a.id.name == name)
        .expect("attribute exists")
}

fn var_list(names: &[&str]) -> String {
    if names.is_empty() {
        return "no variables".to_owned();
    }
    let names = names
        .iter()
        .map(|n| format!("${n}"))
        .collect::<Vec<_>>()
        .join(", ");
    format!("variables {names}")
}

/// Collects errors, dropping exact duplicates (a construct reached through
/// several message references is reported once).
#[derive(Default)]
struct Errors {
    list: Vec<Error>,
}

impl Errors {
    fn push(&mut self, error: Error) {
        if !self.list.contains(&error) {
            self.list.push(error);
        }
    }
}

/// The messages and terms of one locale, by ID.
struct LocaleIndex<'a> {
    /// Indices into the sources, in file name order.
    files: Vec<usize>,
    resources: &'a [ast::Resource<&'a str>],
    messages: HashMap<&'a str, (usize, &'a ast::Message<&'a str>)>,
    terms: HashMap<&'a str, (usize, &'a ast::Term<&'a str>)>,
}

impl<'a> LocaleIndex<'a> {
    fn build(
        locale: usize,
        sources: &'a [Source],
        resources: &'a [ast::Resource<&'a str>],
        errors: &mut Errors,
    ) -> Self {
        let mut index = LocaleIndex {
            files: Vec::new(),
            resources,
            messages: HashMap::new(),
            terms: HashMap::new(),
        };
        for (src, source) in sources
            .iter()
            .enumerate()
            .filter(|(_, s)| s.locale == locale)
        {
            index.files.push(src);
            let stem = source.stem();
            let stem_ok = stem.starts_with(|c: char| c.is_ascii_lowercase())
                && stem
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_');
            if !stem_ok {
                errors.push(whole_file(
                    source.label(),
                    "file names must start with a lowercase letter and use only a-z, 0-9, - and _"
                        .to_owned(),
                ));
            }
            for entry in &resources[src].body {
                match entry {
                    ast::Entry::Message(message) => {
                        let id = message.id.name;
                        let err = |message: String| Error {
                            file: source.label(),
                            line: source.line_of(id),
                            key: Some(id.to_owned()),
                            message,
                        };
                        let rest = id.strip_prefix(stem).and_then(|r| r.strip_prefix('-'));
                        match rest {
                            None => errors.push(err(format!(
                                "IDs in {} must start with `{stem}-`",
                                source.name
                            ))),
                            Some(rest) if !rest.starts_with(|c: char| c.is_ascii_alphabetic()) => {
                                errors.push(err(format!(
                                    "the part after `{stem}-` must start with a letter"
                                )));
                            }
                            Some(_) => {}
                        }
                        for (i, attr) in message.attributes.iter().enumerate() {
                            if message.attributes[..i]
                                .iter()
                                .any(|a| a.id.name == attr.id.name)
                            {
                                errors.push(Error {
                                    file: source.label(),
                                    line: source.line_of(attr.id.name),
                                    key: Some(entry_name(id, Some(attr.id.name))),
                                    message: "duplicate attribute".to_owned(),
                                });
                            }
                        }
                        if let Some(&(other, first)) = index.messages.get(id) {
                            errors.push(err(format!(
                                "duplicate message ID (first defined at {}:{})",
                                sources[other].label(),
                                sources[other].line_of(first.id.name)
                            )));
                        } else {
                            index.messages.insert(id, (src, message));
                        }
                    }
                    ast::Entry::Term(term) => {
                        let id = term.id.name;
                        let err = |message: String| Error {
                            file: source.label(),
                            line: source.line_of(id),
                            key: Some(format!("-{id}")),
                            message,
                        };
                        if !term.attributes.is_empty() {
                            errors.push(err("term attributes are not supported".to_owned()));
                        }
                        if let Some(&(other, first)) = index.terms.get(id) {
                            errors.push(err(format!(
                                "duplicate term ID (first defined at {}:{})",
                                sources[other].label(),
                                sources[other].line_of(first.id.name)
                            )));
                        } else {
                            index.terms.insert(id, (src, term));
                        }
                    }
                    _ => {}
                }
            }
        }
        index
    }

    fn messages_in(&self, src: usize) -> impl Iterator<Item = &'a ast::Message<&'a str>> + 'a {
        let messages = &self.messages;
        let resources: &'a [ast::Resource<&'a str>] = self.resources;
        let firsts: Vec<&'a ast::Message<&'a str>> = resources[src]
            .body
            .iter()
            .filter_map(|e| match e {
                ast::Entry::Message(m) => Some(m),
                _ => None,
            })
            // Skip later duplicates, which are already errors.
            .filter(|m| {
                messages
                    .get(m.id.name)
                    .is_some_and(|&(s, first)| s == src && std::ptr::eq(first, *m))
            })
            .collect();
        firsts.into_iter()
    }
}

/// Same files, messages, attributes and value presence in every locale as in en.
fn check_structure(sources: &[Source], indexes: &[LocaleIndex<'_>], errors: &mut Errors) {
    let en = &indexes[0];
    let en_files: BTreeMap<&str, usize> = en
        .files
        .iter()
        .map(|&s| (sources[s].name.as_str(), s))
        .collect();
    for (locale, index) in indexes.iter().enumerate().skip(1) {
        let tag = LOCALES[locale].tag;
        let files: BTreeMap<&str, usize> = index
            .files
            .iter()
            .map(|&s| (sources[s].name.as_str(), s))
            .collect();
        for name in en_files.keys().filter(|n| !files.contains_key(*n)) {
            errors.push(whole_file(
                format!("{tag}/{name}"),
                format!("missing: en/{name} has no {tag} counterpart"),
            ));
        }
        for (name, &src) in files.iter().filter(|(n, _)| !en_files.contains_key(*n)) {
            errors.push(whole_file(
                sources[src].label(),
                format!("en/{name} does not exist"),
            ));
        }
        for (name, &en_src) in &en_files {
            let Some(&src) = files.get(name) else {
                continue;
            };
            let source = &sources[src];
            let en_source = &sources[en_src];
            for en_message in en.messages_in(en_src) {
                let id = en_message.id.name;
                let at_en = || format!("{}:{}", en_source.label(), en_source.line_of(id));
                let Some(message) = index.messages_in(src).find(|m| m.id.name == id) else {
                    errors.push(Error {
                        file: source.label(),
                        line: 0,
                        key: Some(id.to_owned()),
                        message: format!("missing message (defined at {})", at_en()),
                    });
                    continue;
                };
                let err = |slice: &str, key: String, message: String| Error {
                    file: source.label(),
                    line: source.line_of(slice),
                    key: Some(key),
                    message,
                };
                if en_message.value.is_some() != message.value.is_some() {
                    let has = |m: &ast::Message<&str>| {
                        if m.value.is_some() {
                            "has a value"
                        } else {
                            "has no value"
                        }
                    };
                    errors.push(err(
                        message.id.name,
                        id.to_owned(),
                        format!(
                            "{} here but {} in en ({})",
                            has(message),
                            has(en_message),
                            at_en()
                        ),
                    ));
                }
                for attr in &en_message.attributes {
                    if !message.attributes.iter().any(|a| a.id.name == attr.id.name) {
                        errors.push(err(
                            message.id.name,
                            entry_name(id, Some(attr.id.name)),
                            format!(
                                "missing attribute (defined at {}:{})",
                                en_source.label(),
                                en_source.line_of(attr.id.name)
                            ),
                        ));
                    }
                }
                for attr in &message.attributes {
                    if !en_message
                        .attributes
                        .iter()
                        .any(|a| a.id.name == attr.id.name)
                    {
                        errors.push(err(
                            attr.id.name,
                            entry_name(id, Some(attr.id.name)),
                            format!("attribute is not in en ({})", at_en()),
                        ));
                    }
                }
            }
            for message in index.messages_in(src) {
                if !en.messages_in(en_src).any(|m| m.id.name == message.id.name) {
                    errors.push(Error {
                        file: source.label(),
                        line: source.line_of(message.id.name),
                        key: Some(message.id.name.to_owned()),
                        message: format!("message is not in {}", en_source.label()),
                    });
                }
            }
        }
    }
}

/// One lowered slot of one locale and the variables it uses.
type LoweredEntry = (Vec<Lowered>, Vec<(String, VarType)>);

/// A pattern node before variables are numbered.
#[derive(Clone, Debug)]
enum Lowered {
    Text(String),
    Var(String),
    Select {
        var: String,
        kind: SelectKind,
        arms: Vec<(VariantKey, Vec<Lowered>)>,
        default: usize,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SelectKind {
    Num,
    Str,
    /// Only `*[other]`: no constraint on the selector.
    Untyped,
}

/// Where a pattern being lowered comes from, for error locations.
struct Scope<'s> {
    src: usize,
    /// The message, attribute or term the pattern belongs to.
    key: &'s str,
    /// A slice of its ID in the source, used when nothing more precise exists.
    anchor: &'s str,
    in_term: bool,
}

struct Lowerer<'a, 'e> {
    locale: usize,
    sources: &'a [Source],
    index: &'a LocaleIndex<'a>,
    errors: &'e mut Errors,
    /// Messages and terms already inlined through a reference.
    reached: std::collections::HashSet<String>,
}

impl<'a> Lowerer<'a, '_> {
    fn error(&mut self, scope: &Scope<'_>, at: &str, message: String) {
        let source = &self.sources[scope.src];
        let mut line = source.line_of(at);
        if line == 0 {
            line = source.line_of(scope.anchor);
        }
        self.errors.push(Error {
            file: source.label(),
            line,
            key: Some(scope.key.to_owned()),
            message,
        });
    }

    fn entry(
        &mut self,
        src: usize,
        name: &str,
        anchor: &'a str,
        pattern: &'a ast::Pattern<&'a str>,
    ) -> Vec<Lowered> {
        let scope = Scope {
            src,
            key: name,
            anchor,
            in_term: false,
        };
        let mut out = Vec::new();
        let mut stack = vec![name.to_owned()];
        self.pattern(&mut out, pattern, &scope, &mut stack);
        finish(out)
    }

    /// Lowers a term no message references, only to report errors inside it.
    fn term(&mut self, src: usize, term: &'a ast::Term<&'a str>) {
        let key = format!("-{}", term.id.name);
        if self.reached.contains(&key) {
            return;
        }
        let scope = Scope {
            src,
            key: &key,
            anchor: term.id.name,
            in_term: true,
        };
        let mut out = Vec::new();
        let mut stack = vec![key.clone()];
        self.pattern(&mut out, &term.value, &scope, &mut stack);
    }

    fn pattern(
        &mut self,
        out: &mut Vec<Lowered>,
        pattern: &'a ast::Pattern<&'a str>,
        scope: &Scope<'_>,
        stack: &mut Vec<String>,
    ) {
        for element in &pattern.elements {
            match element {
                ast::PatternElement::TextElement { value } => push_text(out, value),
                ast::PatternElement::Placeable { expression } => {
                    self.expression(out, expression, scope, stack)
                }
            }
        }
    }

    fn expression(
        &mut self,
        out: &mut Vec<Lowered>,
        expression: &'a ast::Expression<&'a str>,
        scope: &Scope<'_>,
        stack: &mut Vec<String>,
    ) {
        match expression {
            ast::Expression::Inline(inline) => self.inline(out, inline, scope, stack),
            ast::Expression::Select { selector, variants } => {
                self.select(out, selector, variants, scope, stack)
            }
        }
    }

    fn inline(
        &mut self,
        out: &mut Vec<Lowered>,
        expression: &'a ast::InlineExpression<&'a str>,
        scope: &Scope<'_>,
        stack: &mut Vec<String>,
    ) {
        use ast::InlineExpression as E;
        match expression {
            E::StringLiteral { value } => push_text(out, &unescape_unicode_to_string(value)),
            E::NumberLiteral { value } => push_text(out, &number_literal_text(value)),
            E::VariableReference { id } => {
                if scope.in_term {
                    self.error(
                        scope,
                        id.name,
                        format!("terms cannot use variables (${})", id.name),
                    );
                } else {
                    out.push(Lowered::Var(id.name.to_owned()));
                }
            }
            E::MessageReference { id, attribute } => {
                let target = entry_name(id.name, attribute.as_ref().map(|a| a.name));
                let Some(&(src, message)) = self.index.messages.get(id.name) else {
                    self.error(scope, id.name, format!("unknown message `{}`", id.name));
                    return;
                };
                let (pattern, anchor) = match attribute {
                    Some(attr) => {
                        match message.attributes.iter().find(|a| a.id.name == attr.name) {
                            Some(a) => (&a.value, a.id.name),
                            None => {
                                self.error(
                                    scope,
                                    id.name,
                                    format!(
                                        "message `{}` has no attribute `{}`",
                                        id.name, attr.name
                                    ),
                                );
                                return;
                            }
                        }
                    }
                    None => match &message.value {
                        Some(value) => (value, message.id.name),
                        None => {
                            self.error(
                                scope,
                                id.name,
                                format!("message `{}` has no value, only attributes", id.name),
                            );
                            return;
                        }
                    },
                };
                self.expand(
                    out,
                    target,
                    Scope {
                        src,
                        key: "",
                        anchor,
                        in_term: scope.in_term,
                    },
                    pattern,
                    scope,
                    id.name,
                    stack,
                );
            }
            E::TermReference {
                id,
                attribute,
                arguments,
            } => {
                if arguments.is_some() {
                    self.error(
                        scope,
                        id.name,
                        format!("term arguments (`-{}(...)`) are not supported", id.name),
                    );
                    return;
                }
                if let Some(attr) = attribute {
                    self.error(
                        scope,
                        id.name,
                        format!(
                            "term attributes (`-{}.{}`) are not supported",
                            id.name, attr.name
                        ),
                    );
                    return;
                }
                let Some(&(src, term)) = self.index.terms.get(id.name) else {
                    self.error(scope, id.name, format!("unknown term `-{}`", id.name));
                    return;
                };
                let target = format!("-{}", id.name);
                self.expand(
                    out,
                    target,
                    Scope {
                        src,
                        key: "",
                        anchor: term.id.name,
                        in_term: true,
                    },
                    &term.value,
                    scope,
                    id.name,
                    stack,
                );
            }
            E::FunctionReference { id, .. } => {
                self.error(
                    scope,
                    id.name,
                    format!("functions such as {}() are not supported; format the value in code and pass it with Args::str", id.name),
                );
            }
            E::Placeable { expression } => self.expression(out, expression, scope, stack),
        }
    }

    /// Inlines a referenced message, attribute or term.
    #[allow(
        clippy::too_many_arguments,
        reason = "internal helper; the arguments are all distinct roles"
    )]
    fn expand(
        &mut self,
        out: &mut Vec<Lowered>,
        target: String,
        inner: Scope<'_>,
        pattern: &'a ast::Pattern<&'a str>,
        scope: &Scope<'_>,
        at: &str,
        stack: &mut Vec<String>,
    ) {
        if stack.contains(&target) {
            let chain = stack
                .iter()
                .chain(std::iter::once(&target))
                .cloned()
                .collect::<Vec<_>>()
                .join(" -> ");
            self.error(scope, at, format!("circular reference: {chain}"));
            return;
        }
        self.reached.insert(target.clone());
        let inner = Scope {
            key: &target,
            ..inner
        };
        stack.push(target.clone());
        let mut nodes = Vec::new();
        self.pattern(&mut nodes, pattern, &inner, stack);
        stack.pop();
        for node in nodes {
            match node {
                Lowered::Text(text) => push_text(out, &text),
                node => out.push(node),
            }
        }
    }

    fn select(
        &mut self,
        out: &mut Vec<Lowered>,
        selector: &'a ast::InlineExpression<&'a str>,
        variants: &'a [ast::Variant<&'a str>],
        scope: &Scope<'_>,
        stack: &mut Vec<String>,
    ) {
        let var = match selector {
            ast::InlineExpression::VariableReference { id } if !scope.in_term => id.name,
            ast::InlineExpression::VariableReference { id } => {
                self.error(
                    scope,
                    id.name,
                    format!("terms cannot use variables (${})", id.name),
                );
                return;
            }
            other => {
                let at = inline_anchor(other).unwrap_or(scope.anchor);
                self.error(
                    scope,
                    at,
                    "select expressions must select on a variable".to_owned(),
                );
                return;
            }
        };
        let has_number = variants
            .iter()
            .any(|v| matches!(v.key, ast::VariantKey::NumberLiteral { .. }));
        let has_string = variants.iter().any(|v| matches!(v.key, ast::VariantKey::Identifier { name } if !CATEGORIES.contains(&name)));
        let has_category =
            variants.iter().any(|v| matches!(v.key, ast::VariantKey::Identifier { name } if name != "other" && CATEGORIES.contains(&name)));
        if has_number && has_string {
            self.error(
                scope,
                var,
                format!("the select on ${var} mixes number keys and string keys"),
            );
        }
        let kind = if has_string {
            SelectKind::Str
        } else if has_number || has_category {
            SelectKind::Num
        } else {
            SelectKind::Untyped
        };
        let spec = &LOCALES[self.locale];
        let mut arms = Vec::with_capacity(variants.len());
        let mut default = 0;
        for (i, variant) in variants.iter().enumerate() {
            if variant.default {
                default = i;
            }
            let key = match variant.key {
                ast::VariantKey::NumberLiteral { value } => {
                    if value.contains('.') {
                        self.error(
                            scope,
                            value,
                            format!("decimal variant keys such as [{value}] are not supported: Fluent never matches them against a number argument"),
                        );
                    }
                    VariantKey::Exact(value.parse().unwrap_or(0.0))
                }
                ast::VariantKey::Identifier { name } if kind == SelectKind::Str => {
                    VariantKey::Str(name.to_owned())
                }
                ast::VariantKey::Identifier { name } => {
                    if !spec.plural_categories.contains(&name) {
                        self.error(
                            scope,
                            name,
                            format!(
                                "plural category [{name}] does not exist in {} (its categories: {})",
                                spec.tag,
                                spec.plural_categories.join(", ")
                            ),
                        );
                    }
                    VariantKey::Plural(
                        CATEGORIES
                            .iter()
                            .find(|c| **c == name)
                            .copied()
                            .unwrap_or("other"),
                    )
                }
            };
            let mut nodes = Vec::new();
            self.pattern(&mut nodes, &variant.value, scope, stack);
            arms.push((key, finish(nodes)));
        }
        out.push(Lowered::Select {
            var: var.to_owned(),
            kind,
            arms,
            default,
        });
    }

    /// Variables of a lowered entry in order of first use, with their types.
    fn vars(
        &mut self,
        src: usize,
        name: &str,
        anchor: &'a str,
        nodes: &[Lowered],
    ) -> Vec<(String, VarType)> {
        let mut vars = Vec::new();
        let mut conflicts = Vec::new();
        collect_vars(nodes, &mut vars, &mut conflicts);
        let scope = Scope {
            src,
            key: name,
            anchor,
            in_term: false,
        };
        for var in conflicts {
            self.error(&scope, anchor, format!("${var} is used both as a number (plural or number keys) and as a string (string keys)"));
        }
        vars
    }
}

fn collect_vars(nodes: &[Lowered], vars: &mut Vec<(String, VarType)>, conflicts: &mut Vec<String>) {
    for node in nodes {
        match node {
            Lowered::Text(_) => {}
            Lowered::Var(name) => note_var(name, VarType::Any, vars, conflicts),
            Lowered::Select {
                var, kind, arms, ..
            } => {
                let ty = match kind {
                    SelectKind::Num => VarType::Num,
                    SelectKind::Str => VarType::Str,
                    SelectKind::Untyped => VarType::Any,
                };
                note_var(var, ty, vars, conflicts);
                for (_, arm) in arms {
                    collect_vars(arm, vars, conflicts);
                }
            }
        }
    }
}

fn note_var(
    name: &str,
    ty: VarType,
    vars: &mut Vec<(String, VarType)>,
    conflicts: &mut Vec<String>,
) {
    match vars.iter_mut().find(|(n, _)| n == name) {
        Some((_, existing)) => match existing.merge(ty) {
            Some(merged) => *existing = merged,
            None => {
                if !conflicts.iter().any(|c| c == name) {
                    conflicts.push(name.to_owned());
                }
            }
        },
        None => vars.push((name.to_owned(), ty)),
    }
}

fn resolve_vars(nodes: Vec<Lowered>, vars: &[(String, VarType)]) -> Vec<Node> {
    let index = |name: &str| {
        vars.iter()
            .position(|(n, _)| n == name)
            .expect("variable sets were checked")
    };
    nodes
        .into_iter()
        .map(|node| match node {
            Lowered::Text(text) => Node::Text(text),
            Lowered::Var(name) => Node::Var(index(&name)),
            Lowered::Select {
                var, arms, default, ..
            } => Node::Select {
                var: index(&var),
                arms: arms
                    .into_iter()
                    .map(|(key, arm)| (key, resolve_vars(arm, vars)))
                    .collect(),
                default,
            },
        })
        .collect()
}

fn push_text(out: &mut Vec<Lowered>, text: &str) {
    if text.is_empty() {
        return;
    }
    if let Some(Lowered::Text(last)) = out.last_mut() {
        last.push_str(text);
    } else {
        out.push(Lowered::Text(text.to_owned()));
    }
}

/// A pattern that renders as nothing still needs one `Text` so that `get`
/// can return it.
fn finish(mut nodes: Vec<Lowered>) -> Vec<Lowered> {
    if nodes.is_empty() {
        nodes.push(Lowered::Text(String::new()));
    }
    nodes
}

/// How fluent-bundle prints a number literal placeable: the value, padded to
/// the number of decimals written.
fn number_literal_text(literal: &str) -> String {
    let value: f64 = literal.parse().unwrap_or(0.0);
    let mut text = value.to_string();
    if let Some(dot) = literal.find('.') {
        let decimals = literal.len() - dot - 1;
        match text.find('.') {
            Some(pos) => {
                let have = text.len() - pos - 1;
                text.push_str(&"0".repeat(decimals.saturating_sub(have)));
            }
            None => {
                text.push('.');
                text.push_str(&"0".repeat(decimals));
            }
        }
    }
    text
}

fn inline_anchor<'a>(expression: &ast::InlineExpression<&'a str>) -> Option<&'a str> {
    use ast::InlineExpression as E;
    match expression {
        E::StringLiteral { value } | E::NumberLiteral { value } => Some(value),
        E::FunctionReference { id, .. }
        | E::MessageReference { id, .. }
        | E::TermReference { id, .. }
        | E::VariableReference { id } => Some(id.name),
        E::Placeable { .. } => None,
    }
}
