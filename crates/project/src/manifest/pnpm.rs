//! pnpm-workspace.yaml: the top-level `packages` key only, as a block
//! sequence or a flow sequence of plain or quoted scalars, with comments.
//!
//! This is a subset of YAML (ADR 0023). Other top-level keys (`catalog`,
//! `overrides`, ...) are skipped by indentation without being parsed.
//! A `packages` value in a form outside the subset (anchors, aliases, tags,
//! block scalars, nested collections) is not expanded and marks the result
//! `partial`.

use super::{Exclude, ManifestError, Member, Parsed, syntax_line};

pub(super) fn parse(text: &str) -> Result<Parsed, ManifestError> {
    let lines: Vec<&str> = text
        .split('\n')
        .map(|l| l.strip_suffix('\r').unwrap_or(l))
        .collect();
    let mut out = Out::default();
    let mut seen = false;
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        let n = i + 1;
        i += 1;
        if is_blank(line) || indent(line) > 0 || is_document_marker(line) || line.starts_with('%') {
            continue;
        }
        let Some((key, rest)) = split_key(line, n)? else {
            continue;
        };
        if key != "packages" {
            continue;
        }
        if seen {
            // js-yaml, which pnpm uses, rejects duplicate keys.
            return Err(syntax_line(n));
        }
        seen = true;
        let rest = strip_comment(rest).trim();
        if rest.is_empty() {
            i = block_sequence(&lines, i, &mut out)?;
        } else if rest.starts_with('[') {
            i = flow_sequence(&lines, n, rest, &mut out)?;
        } else {
            // A scalar, an alias, an anchor, a tag or a block scalar.
            out.partial = true;
        }
    }
    Ok(out.finish())
}

#[derive(Default)]
struct Out {
    items: Vec<String>,
    partial: bool,
}

impl Out {
    fn finish(self) -> Parsed {
        let mut parsed = Parsed {
            workspace: true,
            partial: self.partial,
            ..Parsed::default()
        };
        for item in self.items {
            match item.strip_prefix('!') {
                Some(exclude) => parsed.excludes.push(Exclude::Glob(exclude.into())),
                None => parsed.members.push(Member::Glob(item.into())),
            }
        }
        parsed
    }
}

fn indent(line: &str) -> usize {
    line.len() - line.trim_start_matches(' ').len()
}

fn is_blank(line: &str) -> bool {
    let t = line.trim_start_matches([' ', '\t']);
    t.is_empty() || t.starts_with('#')
}

fn is_document_marker(line: &str) -> bool {
    ["---", "..."].iter().any(|m| {
        line.strip_prefix(m)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with([' ', '\t']))
    })
}

/// A top-level `key:` line: the key and the text after the colon.
/// None when the line is not a mapping key (a sequence item, a continuation).
fn split_key(line: &str, n: usize) -> Result<Option<(String, &str)>, ManifestError> {
    if line.starts_with(['"', '\'']) {
        let (key, rest) = quoted(line).map_err(|()| syntax_line(n))?;
        let rest = rest.trim_start_matches([' ', '\t']);
        return Ok(rest.strip_prefix(':').map(|rest| (key, rest)));
    }
    if line.starts_with(['-', '?', '[', '{', '#']) {
        return Ok(None);
    }
    let bytes = line.as_bytes();
    for (at, &b) in bytes.iter().enumerate() {
        if b == b':' && bytes.get(at + 1).is_none_or(|c| matches!(c, b' ' | b'\t')) {
            return Ok(Some((line[..at].trim_end().to_owned(), &line[at + 1..])));
        }
    }
    Ok(None)
}

/// Removes a trailing `# comment` (a `#` at the start or after whitespace),
/// outside quotes.
fn strip_comment(s: &str) -> &str {
    let bytes = s.as_bytes();
    let mut quote = None;
    let mut at = 0;
    while at < bytes.len() {
        let b = bytes[at];
        match quote {
            Some(b'\'') if b == b'\'' => quote = None,
            Some(b'"') if b == b'\\' => at += 1,
            Some(b'"') if b == b'"' => quote = None,
            Some(_) => {}
            None if b == b'\'' || b == b'"' => quote = Some(b),
            None if b == b'#' && (at == 0 || matches!(bytes[at - 1], b' ' | b'\t')) => {
                return &s[..at];
            }
            None => {}
        }
        at += 1;
    }
    s
}

/// The block sequence under `packages:`, starting at line index `i`.
/// Returns the index of the first line after it.
fn block_sequence(lines: &[&str], mut i: usize, out: &mut Out) -> Result<usize, ManifestError> {
    let mut seq_indent = None;
    while i < lines.len() {
        let line = lines[i];
        let n = i + 1;
        if is_blank(line) {
            i += 1;
            continue;
        }
        let ind = indent(line);
        let content = &line[ind..];
        let is_item = content == "-" || content.starts_with("- ") || content.starts_with("-\t");
        if ind == 0 && (!is_item || is_document_marker(line)) {
            break;
        }
        match seq_indent {
            None if is_item => seq_indent = Some(ind),
            // `packages:` holding a mapping, or text we do not understand.
            None => out.partial = true,
            Some(s) if ind == s && is_item => {}
            // Continuation lines, nested collections.
            Some(_) => out.partial = true,
        }
        if seq_indent == Some(ind) && is_item {
            block_item(content[1..].trim_start_matches([' ', '\t']), n, out)?;
        }
        i += 1;
    }
    Ok(i)
}

fn block_item(value: &str, n: usize, out: &mut Out) -> Result<(), ManifestError> {
    let value = strip_comment(value).trim_end_matches([' ', '\t']);
    if value.is_empty() {
        // A null item: nothing to match.
        return Ok(());
    }
    if value.starts_with(['\'', '"']) {
        let (item, rest) = quoted(value).map_err(|()| syntax_line(n))?;
        if rest.trim().is_empty() {
            out.items.push(item);
        } else {
            out.partial = true;
        }
        return Ok(());
    }
    if value.starts_with(INDICATORS) || value.contains(": ") || value.ends_with(':') {
        out.partial = true;
        return Ok(());
    }
    out.items.push(value.to_owned());
    Ok(())
}

/// Characters that cannot start a plain scalar we accept: aliases, anchors,
/// tags, block scalars, nested collections and reserved indicators.
const INDICATORS: &[char] = &[
    '*', '&', '!', '|', '>', '[', ']', '{', '}', '@', '`', '%', '-', '?', ',',
];

/// A flow sequence starting at `rest` on line `n` (1-based), possibly
/// spanning lines. Returns the index of the first line after it.
fn flow_sequence(
    lines: &[&str],
    n: usize,
    rest: &str,
    out: &mut Out,
) -> Result<usize, ManifestError> {
    // One pass over the rest of the file: the sequence may span lines.
    let mut text = String::from(rest);
    for line in &lines[n..] {
        text.push('\n');
        text.push_str(line);
    }
    let line_of = |s: &str| n + text[..text.len() - s.len()].matches('\n').count();
    match flow_items(&text) {
        Ok((items, partial, trailing)) => {
            let closing_line = line_of(trailing);
            let end = trailing.find('\n').unwrap_or(trailing.len());
            if !strip_comment(&trailing[..end]).trim().is_empty() {
                return Err(syntax_line(closing_line));
            }
            out.items.extend(items);
            out.partial |= partial;
            Ok(closing_line)
        }
        Err(at) => Err(syntax_line(line_of(at))),
    }
}

/// Parses `[a, 'b', "c"]` from the start of `text`: the items, whether some
/// were skipped, and the text after the closing bracket. On error, the text
/// from the point of the error.
fn flow_items(text: &str) -> Result<(Vec<String>, bool, &str), &str> {
    let mut items = Vec::new();
    let mut partial = false;
    let mut s = &text[1..]; // after '['
    loop {
        s = skip_flow_space(s);
        if s.is_empty() {
            // Unterminated: report the line of the opening bracket.
            return Err(text);
        }
        if let Some(trailing) = s.strip_prefix(']') {
            return Ok((items, partial, trailing));
        }
        if let Some(rest) = s.strip_prefix(',') {
            s = rest;
            continue;
        }
        if s.starts_with(['\'', '"']) {
            let Ok((item, rest)) = quoted(s) else {
                return Err(s);
            };
            items.push(item);
            s = rest;
        } else if s.starts_with(['[', '{', '*', '&', '!', '|', '>', '@', '`']) {
            // Nested or unsupported: skip to the next separator at this level.
            partial = true;
            s = skip_nested(s).ok_or(text)?;
        } else {
            let end = s.find([',', ']', '\n']).unwrap_or(s.len());
            let item = strip_comment(&s[..end]).trim_end_matches([' ', '\t']);
            if item.contains(": ") {
                partial = true;
            } else if !item.is_empty() {
                items.push(item.to_owned());
            }
            s = &s[end..];
        }
        // After an item: a separator or the end, possibly after space.
        s = skip_flow_space(s);
        if !(s.is_empty() || s.starts_with([',', ']'])) {
            partial = true;
            s = skip_nested(s).ok_or(text)?;
        }
    }
}

/// Skips whitespace, newlines and comments inside a flow collection.
fn skip_flow_space(mut s: &str) -> &str {
    loop {
        let t = s.trim_start_matches([' ', '\t', '\n', '\r']);
        if t.starts_with('#') {
            s = t.find('\n').map_or("", |at| &t[at..]);
        } else {
            return t;
        }
    }
}

/// Skips a nested flow collection or unsupported node up to the next `,` or
/// `]` of the enclosing sequence. None when the text ends first.
fn skip_nested(s: &str) -> Option<&str> {
    let mut depth = 0usize;
    for (at, c) in s.char_indices() {
        match c {
            '[' | '{' => depth += 1,
            ']' | '}' if depth > 0 => depth -= 1,
            ',' | ']' if depth == 0 => return Some(&s[at..]),
            _ => {}
        }
    }
    None
}

/// A single- or double-quoted scalar at the start of `s`, ending on the same
/// line. Returns the value and the text after the closing quote.
fn quoted(s: &str) -> Result<(String, &str), ()> {
    let mut chars = s.char_indices();
    let Some((_, q)) = chars.next() else {
        return Err(());
    };
    let mut value = String::new();
    while let Some((at, c)) = chars.next() {
        match c {
            '\n' => break,
            '\'' if q == '\'' => {
                if s[at + 1..].starts_with('\'') {
                    value.push('\'');
                    chars.next();
                } else {
                    return Ok((value, &s[at + 1..]));
                }
            }
            '"' if q == '"' => return Ok((value, &s[at + 1..])),
            '\\' if q == '"' => {
                let Some((_, e)) = chars.next() else { break };
                match e {
                    'n' => value.push('\n'),
                    't' => value.push('\t'),
                    '0' => value.push('\0'),
                    'x' | 'u' | 'U' => {
                        let len = match e {
                            'x' => 2,
                            'u' => 4,
                            _ => 8,
                        };
                        let hex: String = chars.by_ref().take(len).map(|(_, c)| c).collect();
                        let decoded = u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32);
                        match decoded {
                            Some(c) if hex.chars().count() == len => value.push(c),
                            _ => return Err(()),
                        }
                    }
                    '\n' => break,
                    other => value.push(other),
                }
            }
            other => value.push(other),
        }
    }
    Err(())
}
