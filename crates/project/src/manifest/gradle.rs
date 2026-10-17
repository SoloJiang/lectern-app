//! settings.gradle (Groovy) and settings.gradle.kts (Kotlin): a lexical scan,
//! not an interpreter (ADR 0023).
//!
//! The lexer skips comments and strings, so only real code is looked at.
//! Recognized, with literal arguments only:
//!
//! - `include`, `includeFlat` and `includeBuild`, in call (`include("a")`)
//!   and Groovy command (`include 'a', 'b'`) form, arguments across lines;
//! - `rootProject.name = "x"`;
//! - `project(":x").projectDir = file("dir")`.
//!
//! The same calls with anything but string literals (variables, loops,
//! interpolated strings, other functions) are not evaluated and mark the
//! result `partial`.

use std::collections::HashMap;
use std::sync::Arc;

use super::{ManifestError, Member, Parsed, arc, syntax_line};

#[derive(Clone, PartialEq, Eq, Debug)]
enum Tok {
    Ident(String),
    /// A string literal; None when it interpolates (`"$x"`, `"${x}"`).
    Str(Option<String>),
    Punct(u8),
    Newline,
    /// Numbers, operators and anything else that does not matter here.
    Other,
}

/// Interpolations nest strings in strings; deeper than this is rejected
/// rather than risking the stack.
const MAX_NESTING: usize = 32;

pub(super) fn parse(text: &str) -> Result<Parsed, ManifestError> {
    let toks = Lexer::new(text.as_bytes()).run()?;
    Ok(Scanner::new(&toks).run())
}

struct Lexer<'a> {
    s: &'a [u8],
    at: usize,
    line: usize,
}

impl<'a> Lexer<'a> {
    fn new(s: &'a [u8]) -> Self {
        Self { s, at: 0, line: 1 }
    }

    fn peek(&self, k: usize) -> Option<u8> {
        self.s.get(self.at + k).copied()
    }

    fn starts_with(&self, p: &[u8]) -> bool {
        self.s[self.at..].starts_with(p)
    }

    fn run(mut self) -> Result<Vec<Tok>, ManifestError> {
        let mut toks = Vec::new();
        if self.starts_with(b"#!") {
            self.skip_line();
        }
        while let Some(c) = self.peek(0) {
            match c {
                b'\n' => {
                    self.at += 1;
                    self.line += 1;
                    toks.push(Tok::Newline);
                }
                b' ' | b'\t' | b'\r' | 0x0c => self.at += 1,
                b'\\' if self.peek(1) == Some(b'\n') => {
                    // Line continuation: the statement goes on.
                    self.at += 2;
                    self.line += 1;
                }
                b'/' if self.peek(1) == Some(b'/') => self.skip_line(),
                b'/' if self.peek(1) == Some(b'*') => self.skip_block_comment()?,
                b'"' | b'\'' => toks.push(Tok::Str(self.string(0)?)),
                b'`' => {
                    // Kotlin backquoted identifier.
                    let start = self.at + 1;
                    let end = self.s[start..]
                        .iter()
                        .position(|&b| b == b'`' || b == b'\n')
                        .map_or(self.s.len(), |p| start + p);
                    toks.push(Tok::Ident(
                        String::from_utf8_lossy(&self.s[start..end]).into_owned(),
                    ));
                    self.at = (end + 1).min(self.s.len());
                }
                c if c.is_ascii_alphabetic() || c == b'_' || c == b'$' => {
                    let start = self.at;
                    while self
                        .peek(0)
                        .is_some_and(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'$')
                    {
                        self.at += 1;
                    }
                    toks.push(Tok::Ident(
                        String::from_utf8_lossy(&self.s[start..self.at]).into_owned(),
                    ));
                }
                c if c.is_ascii_digit() => {
                    while self
                        .peek(0)
                        .is_some_and(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'.')
                    {
                        self.at += 1;
                    }
                    toks.push(Tok::Other);
                }
                b'?' if self.peek(1) == Some(b'.') => {
                    // Safe navigation is still member access.
                    self.at += 2;
                    toks.push(Tok::Punct(b'.'));
                }
                b'=' | b'!' | b'<' | b'>' | b'+' | b'-' | b'*' | b'/' | b'%' | b'&' | b'|'
                | b'^' | b'~' | b'?' | b':' => {
                    // `=` alone is assignment; with another operator
                    // character (`==`, `+=`, `->`) it is something else.
                    let op = self.at;
                    self.at += 1;
                    while self.peek(0).is_some_and(|b| {
                        matches!(b, b'=' | b'<' | b'>' | b'&' | b'|' | b'~' | b':')
                    }) {
                        self.at += 1;
                    }
                    toks.push(if &self.s[op..self.at] == b"=" {
                        Tok::Punct(b'=')
                    } else {
                        Tok::Other
                    });
                }
                b'(' | b')' | b'[' | b']' | b'{' | b'}' | b',' | b'.' | b';' => {
                    self.at += 1;
                    toks.push(Tok::Punct(c));
                }
                _ => {
                    self.at += 1;
                    toks.push(Tok::Other);
                }
            }
        }
        Ok(toks)
    }

    fn skip_line(&mut self) {
        while self.peek(0).is_some_and(|b| b != b'\n') {
            self.at += 1;
        }
    }

    fn skip_block_comment(&mut self) -> Result<(), ManifestError> {
        let start = self.line;
        self.at += 2;
        loop {
            match self.peek(0) {
                None => return Err(syntax_line(start)),
                Some(b'*') if self.peek(1) == Some(b'/') => {
                    self.at += 2;
                    return Ok(());
                }
                Some(b'\n') => {
                    self.line += 1;
                    self.at += 1;
                }
                Some(_) => self.at += 1,
            }
        }
    }

    /// A string starting at the current quote. Its value, or None when it
    /// interpolates. `depth` counts the strings it is nested in.
    fn string(&mut self, depth: usize) -> Result<Option<String>, ManifestError> {
        let start = self.line;
        if depth > MAX_NESTING {
            return Err(syntax_line(start));
        }
        let q = self.s[self.at];
        let triple = self.peek(1) == Some(q) && self.peek(2) == Some(q);
        if !triple && self.peek(1) == Some(q) {
            self.at += 2;
            return Ok(Some(String::new()));
        }
        self.at += if triple { 3 } else { 1 };
        let mut value = Vec::new();
        let mut interpolated = false;
        loop {
            let Some(c) = self.peek(0) else {
                return Err(syntax_line(start));
            };
            if c == q && (!triple || (self.peek(1) == Some(q) && self.peek(2) == Some(q))) {
                self.at += if triple { 3 } else { 1 };
                break;
            }
            match c {
                b'\n' if !triple => return Err(syntax_line(start)),
                b'\n' => {
                    self.line += 1;
                    self.at += 1;
                    value.push(c);
                }
                b'\\' => {
                    self.at += 1;
                    let Some(e) = self.peek(0) else {
                        return Err(syntax_line(start));
                    };
                    self.at += 1;
                    match e {
                        b'n' => value.push(b'\n'),
                        b't' => value.push(b'\t'),
                        b'r' => value.push(b'\r'),
                        b'b' => value.push(0x08),
                        b'f' => value.push(0x0c),
                        b'u' => {
                            let hex = self.s.get(self.at..self.at + 4).unwrap_or_default();
                            let decoded = std::str::from_utf8(hex)
                                .ok()
                                .and_then(|h| u32::from_str_radix(h, 16).ok())
                                .and_then(char::from_u32);
                            match decoded {
                                Some(ch) => {
                                    self.at += 4;
                                    value.extend_from_slice(ch.encode_utf8(&mut [0; 4]).as_bytes());
                                }
                                None => value.push(b'u'),
                            }
                        }
                        b'\n' => self.line += 1,
                        other => value.push(other),
                    }
                }
                b'$' if q == b'"' && self.peek(1) == Some(b'{') => {
                    interpolated = true;
                    self.at += 2;
                    self.template_expression(start, depth)?;
                }
                b'$' if q == b'"'
                    && self
                        .peek(1)
                        .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_') =>
                {
                    interpolated = true;
                    self.at += 1;
                }
                _ => {
                    self.at += 1;
                    value.push(c);
                }
            }
        }
        Ok((!interpolated).then(|| String::from_utf8_lossy(&value).into_owned()))
    }

    /// Skips `...}` after `${`, including strings nested in it.
    fn template_expression(&mut self, start: usize, depth: usize) -> Result<(), ManifestError> {
        let mut braces = 1usize;
        loop {
            match self.peek(0) {
                None => return Err(syntax_line(start)),
                Some(b'{') => {
                    braces += 1;
                    self.at += 1;
                }
                Some(b'}') => {
                    self.at += 1;
                    braces -= 1;
                    if braces == 0 {
                        return Ok(());
                    }
                }
                Some(b'"' | b'\'') => {
                    self.string(depth + 1)?;
                }
                Some(b'\n') => {
                    self.line += 1;
                    self.at += 1;
                }
                Some(_) => self.at += 1,
            }
        }
    }
}

/// Finds the recognized statements in the token stream.
struct Scanner<'a> {
    toks: &'a [Tok],
    name: Option<Arc<str>>,
    /// Gradle project paths (`:a:b`) from `include`, and their directories.
    projects: Vec<(String, String)>,
    /// `includeFlat` and `includeBuild` members, in order.
    others: Vec<Member>,
    /// Project path → `projectDir` override.
    dirs: HashMap<String, String>,
    partial: bool,
}

/// One argument: its literal value, or None when it is not a string literal.
type Arg = Option<String>;

impl<'a> Scanner<'a> {
    fn new(toks: &'a [Tok]) -> Self {
        Self {
            toks,
            name: None,
            projects: Vec::new(),
            others: Vec::new(),
            dirs: HashMap::new(),
            partial: false,
        }
    }

    fn tok(&self, i: usize) -> Option<&'a Tok> {
        self.toks.get(i)
    }

    fn is_punct(&self, i: usize, p: u8) -> bool {
        self.tok(i) == Some(&Tok::Punct(p))
    }

    fn is_ident(&self, i: usize, name: &str) -> bool {
        matches!(self.tok(i), Some(Tok::Ident(n)) if n == name)
    }

    /// Whether token `i` ends a statement.
    fn is_end(&self, i: usize) -> bool {
        matches!(
            self.tok(i),
            None | Some(Tok::Newline | Tok::Punct(b';' | b'}'))
        )
    }

    fn run(mut self) -> Parsed {
        let mut i = 0;
        while i < self.toks.len() {
            let member_access = i > 0 && self.is_punct(i - 1, b'.');
            i = match self.tok(i) {
                Some(Tok::Ident(id)) if !member_access => match id.as_str() {
                    "include" | "includeFlat" | "includeBuild" => self.include(i, id),
                    "rootProject" => self.root_name(i),
                    "project" => self.project_dir(i),
                    _ => i + 1,
                },
                _ => i + 1,
            };
        }
        self.finish()
    }

    fn finish(self) -> Parsed {
        let mut members = Vec::new();
        for (path, dir) in self.projects {
            let name = path.rsplit(':').next().map(arc);
            let dir = self.dirs.get(&path).cloned().unwrap_or(dir);
            members.push(Member::Path {
                path: dir.into(),
                name,
            });
        }
        members.extend(self.others);
        Parsed {
            name: self.name,
            group: None,
            workspace: true,
            members,
            excludes: Vec::new(),
            partial: self.partial,
        }
    }

    /// `include`, `includeFlat` or `includeBuild` at `i`.
    fn include(&mut self, i: usize, verb: &str) -> usize {
        // `include = ...` or `include.x`: not a call.
        if self.is_punct(i + 1, b'=') || self.is_punct(i + 1, b'.') {
            return i + 1;
        }
        let (args, next) = self.args(i + 1);
        let args = if verb == "includeBuild" {
            &args[..args.len().min(1)]
        } else {
            &args[..]
        };
        for arg in args {
            let Some(value) = arg else {
                self.partial = true;
                continue;
            };
            match verb {
                "include" => {
                    if let Some(path) = project_path(value) {
                        let dir = path.trim_start_matches(':').replace(':', "/");
                        self.projects.push((path, dir));
                    }
                }
                "includeFlat" => {
                    if let Some(path) = project_path(value) {
                        let name = path.rsplit(':').next().unwrap_or_default();
                        self.others.push(Member::Path {
                            path: format!("../{name}").into(),
                            name: Some(arc(name)),
                        });
                    }
                }
                _ => self.others.push(Member::Path {
                    path: value.as_str().into(),
                    name: None,
                }),
            }
        }
        next
    }

    /// `rootProject.name = "x"` at `i`.
    fn root_name(&mut self, i: usize) -> usize {
        if !(self.is_punct(i + 1, b'.')
            && self.is_ident(i + 2, "name")
            && self.is_punct(i + 3, b'='))
        {
            return i + 1;
        }
        match self.tok(i + 4) {
            Some(Tok::Str(Some(name))) if self.is_end(i + 5) => self.name = Some(arc(name)),
            _ => self.partial = true,
        }
        i + 4
    }

    /// `project(":x").projectDir = file("dir")` at `i`.
    fn project_dir(&mut self, i: usize) -> usize {
        if !self.is_punct(i + 1, b'(') {
            return i + 1;
        }
        let (args, after) = self.args(i + 1);
        if !(self.is_punct(after, b'.')
            && self.is_ident(after + 1, "projectDir")
            && self.is_punct(after + 2, b'='))
        {
            // Another use of `project(...)`; its arguments hold no statements.
            return after;
        }
        let project = match args.as_slice() {
            [Some(p)] => project_path(p),
            _ => None,
        };
        let value = after + 3;
        let mut next = value;
        let dir = if self.is_ident(value, "file") && self.is_punct(value + 1, b'(') {
            let (args, end) = self.args(value + 1);
            next = end;
            match args.as_slice() {
                [Some(dir)] if self.is_end(end) => Some(dir.clone()),
                _ => None,
            }
        } else {
            None
        };
        match (project, dir) {
            (Some(project), Some(dir)) => {
                self.dirs.insert(project, dir);
            }
            _ => self.partial = true,
        }
        next
    }

    /// The arguments of a call whose argument list starts at `i`: in
    /// parentheses, or Groovy command arguments up to the end of the
    /// statement. Returns them and the index after them.
    fn args(&self, i: usize) -> (Vec<Arg>, usize) {
        let mut args = Vec::new();
        let mut current: Vec<&Tok> = Vec::new();
        let paren = self.is_punct(i, b'(');
        let mut depth = 0usize;
        let mut j = if paren { i + 1 } else { i };
        let push = |current: &mut Vec<&Tok>, args: &mut Vec<Arg>| {
            if !current.is_empty() {
                args.push(match current.as_slice() {
                    [Tok::Str(s)] => s.clone(),
                    _ => None,
                });
                current.clear();
            }
        };
        while let Some(t) = self.tok(j) {
            match t {
                Tok::Punct(b'(' | b'[' | b'{') => depth += 1,
                Tok::Punct(b')' | b']' | b'}') if depth > 0 => depth -= 1,
                Tok::Punct(b')') if paren => {
                    push(&mut current, &mut args);
                    return (args, j + 1);
                }
                Tok::Punct(b',') if depth == 0 => {
                    push(&mut current, &mut args);
                    j += 1;
                    continue;
                }
                Tok::Newline if paren || depth > 0 || (current.is_empty() && !args.is_empty()) => {
                    // Inside parentheses, or right after a comma.
                    j += 1;
                    continue;
                }
                _ if !paren && depth == 0 && self.is_end(j) => break,
                _ => {}
            }
            if !matches!(t, Tok::Newline) {
                current.push(t);
            }
            j += 1;
        }
        push(&mut current, &mut args);
        (args, j)
    }
}

/// A Gradle project path in canonical `:a:b` form; None when it names no
/// project (`""`, `":"`).
fn project_path(value: &str) -> Option<String> {
    let segments: Vec<&str> = value.split(':').filter(|s| !s.is_empty()).collect();
    (!segments.is_empty()).then(|| format!(":{}", segments.join(":")))
}
