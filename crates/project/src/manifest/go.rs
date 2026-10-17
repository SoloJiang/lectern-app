//! go.work (`use`) and go.mod (`module`), following the line-oriented syntax
//! of Go's modfile package: one directive per line or a parenthesized block,
//! `//` comments, interpreted ("...") and raw (`...`) strings.
//! Other directives (`go`, `toolchain`, `replace`, `require`, ...) are
//! skipped.

use super::{ManifestError, Member, Parsed, arc, syntax_line};

#[derive(Clone, PartialEq, Eq, Debug)]
enum Tok {
    /// A bare word or the value of a quoted string.
    Word(String),
    Open,
    Close,
}

/// One directive: the verb, its arguments and its 1-based line.
struct Directive {
    verb: String,
    args: Vec<String>,
    line: usize,
}

pub(super) fn parse_work(text: &str) -> Result<Parsed, ManifestError> {
    let mut parsed = Parsed {
        workspace: true,
        ..Parsed::default()
    };
    for d in directives(text)? {
        if d.verb == "use"
            && let Some(path) = d.args.into_iter().next()
        {
            parsed.members.push(Member::Path {
                path: path.into(),
                name: None,
            });
        }
    }
    Ok(parsed)
}

pub(super) fn parse_mod(text: &str) -> Result<Parsed, ManifestError> {
    let mut parsed = Parsed::default();
    for d in directives(text)? {
        if d.verb != "module" {
            continue;
        }
        if parsed.name.is_some() {
            // The go command rejects a repeated module statement.
            return Err(syntax_line(d.line));
        }
        let Some(path) = d.args.first() else {
            return Err(syntax_line(d.line));
        };
        parsed.name = Some(arc(path));
    }
    Ok(parsed)
}

fn directives(text: &str) -> Result<Vec<Directive>, ManifestError> {
    let mut out = Vec::new();
    // The verb of the open block and the line it opened on.
    let mut block: Option<(String, usize)> = None;
    for (index, line) in text.split('\n').enumerate() {
        let n = index + 1;
        let toks = lex_line(line).map_err(|()| syntax_line(n))?;
        if toks.is_empty() {
            continue;
        }
        if let Some((verb, _)) = &block {
            match toks.as_slice() {
                [Tok::Close] => block = None,
                _ => {
                    let args = words(&toks).ok_or_else(|| syntax_line(n))?;
                    out.push(Directive {
                        verb: verb.clone(),
                        args,
                        line: n,
                    });
                }
            }
            continue;
        }
        match toks.as_slice() {
            [Tok::Word(verb), Tok::Open] => block = Some((verb.clone(), n)),
            [Tok::Word(verb), rest @ ..] => {
                let args = words(rest).ok_or_else(|| syntax_line(n))?;
                out.push(Directive {
                    verb: verb.clone(),
                    args,
                    line: n,
                });
            }
            _ => return Err(syntax_line(n)),
        }
    }
    if let Some((_, start)) = block {
        return Err(syntax_line(start));
    }
    Ok(out)
}

/// The arguments as words; None when a parenthesis appears among them.
fn words(toks: &[Tok]) -> Option<Vec<String>> {
    toks.iter()
        .map(|t| match t {
            Tok::Word(w) => Some(w.clone()),
            Tok::Open | Tok::Close => None,
        })
        .collect()
}

/// The tokens of one line. Err for an unterminated or malformed string.
fn lex_line(line: &str) -> Result<Vec<Tok>, ()> {
    let mut toks = Vec::new();
    let mut chars = line.char_indices().peekable();
    while let Some(&(at, c)) = chars.peek() {
        match c {
            c if c.is_whitespace() => {
                chars.next();
            }
            '/' if line[at..].starts_with("//") => break,
            '(' => {
                chars.next();
                toks.push(Tok::Open);
            }
            ')' => {
                chars.next();
                toks.push(Tok::Close);
            }
            '`' => {
                chars.next();
                let rest = &line[at + 1..];
                let end = rest.find('`').ok_or(())?;
                toks.push(Tok::Word(rest[..end].to_owned()));
                // Skip to just after the closing backquote.
                while chars.next_if(|&(i, _)| i <= at + 1 + end).is_some() {}
            }
            '"' => {
                chars.next();
                let mut value = String::new();
                loop {
                    let (_, c) = chars.next().ok_or(())?;
                    match c {
                        '"' => break,
                        '\\' => {
                            let (_, e) = chars.next().ok_or(())?;
                            value.push(match e {
                                'n' => '\n',
                                't' => '\t',
                                'r' => '\r',
                                other => other,
                            });
                        }
                        other => value.push(other),
                    }
                }
                toks.push(Tok::Word(value));
            }
            _ => {
                let rest = &line[at..];
                let end = rest
                    .find(|c: char| c.is_whitespace() || matches!(c, '(' | ')' | '"' | '`'))
                    .unwrap_or(rest.len());
                let word = &rest[..end];
                // A comment may follow a word directly.
                let word = word.find("//").map_or(word, |p| &word[..p]);
                toks.push(Tok::Word(word.to_owned()));
                let stop = at + word.len();
                while chars.next_if(|&(i, _)| i < stop).is_some() {}
                if word.len() < end {
                    break;
                }
            }
        }
    }
    Ok(toks)
}
