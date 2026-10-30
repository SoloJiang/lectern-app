//! The prompt a batch of review notes is sent as (docs/design/review.md「prompt
//! 的模板」). English and fixed, whatever the interface language: its reader
//! is an agent. The text of a note is kept as the user wrote it. Pure: no
//! IO, no model name, nothing logged.

use std::fmt::Write as _;

use lectern_services::changes::ChangePath;
use lectern_services::review::{BatchId, Excerpt};

#[cfg(test)]
mod tests;

/// The version of the template. A change of the format raises it.
pub const TEMPLATE_VERSION: u32 = 1;

/// Where the anchor's line is, as the agent can find it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Where {
    /// The new side, and it is the content the worktree has now.
    Current,
    /// The old side, and it is the content of the base.
    Base,
    /// Anything else: an earlier version in the narrative, a line the code
    /// moved away from (Drifted), a file with no change any more.
    Earlier,
}

/// One note of a batch, in the order they are written in the prompt.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct PromptNote {
    /// Relative to the worktree root: the new path, the old one for a file
    /// that was deleted.
    pub path: ChangePath,
    /// The anchor's line, 0-based.
    pub line: u32,
    pub place: Where,
    pub excerpt: Option<Excerpt>,
    pub text: std::sync::Arc<str>,
}

/// How many characters of the first line the batch marker lies in: the
/// agents keep the first 120 characters of a prompt's first line as its
/// preview, and the marker must be within them
/// ("与回应的那一轮对应").
const MARKER_WITHIN: usize = 60;

/// `[lectern-notes <16 hex digits of the batch ID>]`: the text that links a
/// turn of the session to the batch it answers.
pub fn marker(batch: BatchId) -> String {
    format!("[lectern-notes {:016x}]", batch.0)
}

/// The prompt of `notes` as one batch. The first line carries the marker;
/// the text ends with the last note's text, with no newline after it.
pub fn prompt(batch: BatchId, notes: &[PromptNote]) -> String {
    let mut out = String::with_capacity(160 + notes.iter().map(size_hint).sum::<usize>());
    let _ = write!(
        out,
        "Review notes from Lectern {}: {} {} on your changes.\nPlease address each note.",
        marker(batch),
        notes.len(),
        if notes.len() == 1 { "note" } else { "notes" },
    );
    debug_assert!(out.find(']').is_some_and(|i| i < MARKER_WITHIN));
    for (i, note) in notes.iter().enumerate() {
        out.push_str("\n\n");
        write_note(&mut out, i + 1, note);
    }
    out
}

fn size_hint(note: &PromptNote) -> usize {
    note.text.len()
        + note.path.0.len()
        + 120
        + note
            .excerpt
            .as_ref()
            .map_or(0, |e| e.lines.iter().map(|l| l.len() + 16).sum())
}

fn write_note(out: &mut String, n: usize, note: &PromptNote) {
    let path = String::from_utf8_lossy(&note.path.0);
    let line = u64::from(note.line) + 1;
    let _ = match note.place {
        Where::Current => write!(out, "{n}. {path}:{line}"),
        Where::Base => write!(
            out,
            "{n}. {path} (line {line} of the version before your changes)"
        ),
        Where::Earlier => write!(
            out,
            "{n}. {path} (line {line} when this note was written; the code may have changed since)"
        ),
    };
    if let Some(excerpt) = note.excerpt.as_ref().filter(|e| !e.lines.is_empty()) {
        out.push_str("\n   ");
        write_excerpt(out, excerpt, note.line);
    }
    out.push_str("\n   Note:");
    // The text as written; a line break at its end would leave an empty
    // line before the next entry or at the end of the prompt.
    let text = note.text.trim_end_matches(['\n', '\r']);
    for line in text.lines() {
        out.push('\n');
        if !line.is_empty() {
            out.push_str("   ");
            out.push_str(line);
        }
    }
}

/// The fence with the lines of the excerpt: line numbers right-aligned to
/// one width, `> ` before the anchor's line.
fn write_excerpt(out: &mut String, excerpt: &Excerpt, anchor: u32) {
    let longest = excerpt
        .lines
        .iter()
        .map(|l| longest_run(l, '`'))
        .max()
        .unwrap_or(0);
    let fence = "`".repeat((longest + 1).max(3));
    let last = u64::from(excerpt.first) + excerpt.lines.len() as u64;
    let width = last.to_string().len();
    out.push_str(&fence);
    for (k, text) in excerpt.lines.iter().enumerate() {
        let at = u64::from(excerpt.first) + k as u64;
        let mark = if at == u64::from(anchor) { "> " } else { "  " };
        let _ = write!(out, "\n   {mark}{:>width$} | {text}", at + 1);
    }
    let _ = write!(out, "\n   {fence}");
}

fn longest_run(text: &str, c: char) -> usize {
    let (mut best, mut run) = (0, 0);
    for ch in text.chars() {
        if ch == c {
            run += 1;
            best = best.max(run);
        } else {
            run = 0;
        }
    }
    best
}
