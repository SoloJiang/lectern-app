//! `verbose.json`: verbose logging turned on in the settings, kept across
//! restarts until it expires. Read and written by the writer thread only.
//!
//! `{"filter":"debug","until":"2026-10-23T03:27:01Z"}`

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::engine::time;
use crate::filter::Filter;

pub(crate) fn path(dir: &Path) -> PathBuf {
    dir.join("verbose.json")
}

/// The filter as the header and `verbose.json` write it: characters
/// outside `[a-z0-9_=,@:-]` become `?`.
pub(crate) fn filter_text(filter: &Filter) -> String {
    filter
        .to_string()
        .chars()
        .map(|c| match c {
            'a'..='z' | '0'..='9' | '_' | '=' | ',' | '@' | ':' | '-' => c,
            _ => '?',
        })
        .collect()
}

pub(crate) fn encode(filter: &Filter, until: SystemTime) -> String {
    format!(
        "{{\"filter\":\"{}\",\"until\":\"{}\"}}\n",
        filter_text(filter),
        time::rfc3339_s(until)
    )
}

/// None when the file is not what [`encode`] writes, or its filter has a
/// directive that does not parse.
pub(crate) fn decode(text: &str) -> Option<(Filter, SystemTime)> {
    let body = text.trim().strip_prefix('{')?.strip_suffix('}')?;
    let mut filter = None;
    let mut until = None;
    for pair in body.split(',').fold(Vec::<String>::new(), |mut acc, part| {
        // Filter values contain commas: a part without `":"` continues the
        // previous value.
        match acc.last_mut() {
            Some(last) if !part.contains("\":") => {
                last.push(',');
                last.push_str(part);
            }
            _ => acc.push(part.to_owned()),
        }
        acc
    }) {
        let (key, value) = pair.split_once(':')?;
        let key = key.trim().strip_prefix('"')?.strip_suffix('"')?;
        let value = value.trim().strip_prefix('"')?.strip_suffix('"')?;
        match key {
            "filter" => filter = Some(value.to_owned()),
            "until" => until = Some(time::parse_rfc3339_s(value)?),
            _ => {}
        }
    }
    let (filter, skipped) = Filter::parse(&filter?);
    skipped.is_empty().then_some((filter, until?))
}
