//! pom.xml, read as a stream: `artifactId`, `groupId` (or the parent's),
//! `modules/module` (POM 4.0.0) and `subprojects/subproject` (POM 4.1.0),
//! including those inside `profiles/profile`. All profiles count, since which
//! one is active cannot be known without running Maven.

use quick_xml::Reader;
use quick_xml::events::{BytesRef, Event};

use super::{ManifestError, Member, Parsed, arc, syntax_at};

/// The element whose text is being collected.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Field {
    ArtifactId,
    GroupId,
    ParentGroupId,
    Module,
}

pub(super) fn parse(text: &str) -> Result<Parsed, ManifestError> {
    let mut reader = Reader::from_str(text);
    let error = |reader: &Reader<&[u8]>| {
        syntax_at(
            text.as_bytes(),
            usize::try_from(reader.error_position()).unwrap_or(usize::MAX),
        )
    };

    let mut parsed = Parsed::default();
    let mut parent_group = None;
    // Local names of the open elements, outermost first.
    let mut stack: Vec<String> = Vec::new();
    let mut seen_root = false;
    let mut is_project = false;
    // The field being read and its text so far.
    let mut field: Option<(Field, usize, String)> = None;

    loop {
        let event = reader.read_event().map_err(|_| error(&reader))?;
        match event {
            Event::Start(start) => {
                let name = start.local_name().as_ref().to_owned();
                if stack.is_empty() {
                    seen_root = true;
                    is_project = name == "project";
                }
                stack.push(name);
                if is_project {
                    if let Some(f) = field_at(&stack) {
                        if matches!(f, Field::Module) {
                            parsed.workspace = true;
                        }
                        field = Some((f, stack.len(), String::new()));
                    } else if is_container(&stack) {
                        parsed.workspace = true;
                    }
                }
            }
            Event::Empty(start) => {
                let name = start.local_name().as_ref().to_owned();
                if stack.is_empty() {
                    seen_root = true;
                    is_project = name == "project";
                }
                stack.push(name);
                if is_project && is_container(&stack) {
                    parsed.workspace = true;
                }
                stack.pop();
            }
            Event::End(_) => {
                if let Some((f, _, value)) = field.take_if(|(_, depth, _)| *depth == stack.len()) {
                    store(
                        &mut parsed,
                        &mut parent_group,
                        f,
                        value.trim_matches(is_xml_space),
                    );
                }
                stack.pop();
            }
            Event::Text(t) => append(&mut field, &stack, &t),
            Event::CData(t) => append(&mut field, &stack, &t),
            Event::GeneralRef(r) => append(&mut field, &stack, &resolve_ref(&r)),
            Event::Eof => break,
            Event::Comment(_) | Event::Decl(_) | Event::PI(_) | Event::DocType(_) => {}
        }
    }
    if !stack.is_empty() || !seen_root {
        return Err(syntax_at(text.as_bytes(), text.len()));
    }
    if parsed.group.is_none() {
        parsed.group = parent_group;
    }
    Ok(parsed)
}

fn is_xml_space(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\r' | '\n')
}

/// The field an element at this position holds, if any.
fn field_at(stack: &[String]) -> Option<Field> {
    // Every field is at most five levels deep; deeper elements cost nothing.
    if stack.len() > 5 {
        return None;
    }
    let path: Vec<&str> = stack.iter().map(String::as_str).collect();
    match path.as_slice() {
        ["project", "artifactId"] => Some(Field::ArtifactId),
        ["project", "groupId"] => Some(Field::GroupId),
        ["project", "parent", "groupId"] => Some(Field::ParentGroupId),
        ["project", "modules", "module"]
        | ["project", "subprojects", "subproject"]
        | ["project", "profiles", "profile", "modules", "module"]
        | [
            "project",
            "profiles",
            "profile",
            "subprojects",
            "subproject",
        ] => Some(Field::Module),
        _ => None,
    }
}

/// `modules` or `subprojects`, at the top level or in a profile.
fn is_container(stack: &[String]) -> bool {
    if stack.len() > 4 {
        return false;
    }
    let path: Vec<&str> = stack.iter().map(String::as_str).collect();
    matches!(
        path.as_slice(),
        ["project", "modules" | "subprojects"]
            | ["project", "profiles", "profile", "modules" | "subprojects"]
    )
}

/// Adds text to the open field when the text is its direct content.
fn append(field: &mut Option<(Field, usize, String)>, stack: &[String], text: &str) {
    if let Some((_, depth, value)) = field
        && *depth == stack.len()
    {
        value.push_str(text);
    }
}

/// The text of `&name;`: the predefined entities and character references.
/// Anything else stays as written.
fn resolve_ref(r: &BytesRef<'_>) -> String {
    if let Ok(Some(c)) = r.resolve_char_ref() {
        return c.to_string();
    }
    match r.as_ref() {
        "lt" => "<".into(),
        "gt" => ">".into(),
        "amp" => "&".into(),
        "apos" => "'".into(),
        "quot" => "\"".into(),
        other => format!("&{other};"),
    }
}

fn store(
    parsed: &mut Parsed,
    parent_group: &mut Option<std::sync::Arc<str>>,
    field: Field,
    value: &str,
) {
    if value.is_empty() {
        return;
    }
    match field {
        Field::ArtifactId => parsed.name = Some(arc(value)),
        Field::GroupId => parsed.group = Some(arc(value)),
        Field::ParentGroupId => *parent_group = Some(arc(value)),
        Field::Module => {
            // Maven interpolates properties into module paths; we do not.
            if value.contains("${") {
                parsed.partial = true;
            } else {
                parsed.members.push(Member::Path {
                    path: value.into(),
                    name: None,
                });
            }
        }
    }
}
