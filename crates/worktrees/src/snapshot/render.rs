//! A composition as text, for the insta snapshots of the `snapshot_*` tests:
//! the whole model, one line per row, with no addresses that change.

use std::fmt::Write;

use lectern_services::session::Agent;
use lectern_services::usage::UsageQuery;

use super::{Built, HeadLabel, OverlapRow, Overlaps, RowUsage, WorktreeRow, WorktreesSnapshot};
use crate::testing::{DAY, MINUTE, NOW};

pub(crate) fn render(built: &Built) -> String {
    let mut out = String::new();
    let w = &mut out;
    let snapshot: &WorktreesSnapshot = &built.snapshot;
    let _ = writeln!(
        w,
        "snapshot folder={} settled={} switches={} base={}",
        snapshot.folder,
        snapshot.files_settled,
        snapshot.switches,
        snapshot
            .base
            .as_ref()
            .map_or("-".to_owned(), |base| format!(
                "{} ({})",
                base.short, base.full
            )),
    );
    let _ = writeln!(
        w,
        "sources project={} sessions={:?} changes={:?} usage={}",
        snapshot.sources.project,
        snapshot
            .sources
            .sessions
            .iter()
            .map(|(id, generation)| format!("{}@{generation}", id.0))
            .collect::<Vec<_>>(),
        snapshot.sources.changes,
        snapshot.sources.usage,
    );
    let _ = writeln!(
        w,
        "built_at={} next_expiry={}",
        snapshot.built_at.0 - NOW,
        snapshot
            .next_expiry
            .map_or("-".to_owned(), |at| format!("{}ms", at.0 - NOW)),
    );
    let _ = writeln!(w, "active {}", snapshot.active_rows.len());
    for row in snapshot.active_rows.iter() {
        write_row(w, row);
    }
    let _ = writeln!(w, "idle {}", snapshot.idle_rows.len());
    for row in snapshot.idle_rows.iter() {
        write_row(w, row);
    }
    let _ = writeln!(
        w,
        "pending {:?}",
        built.pending.iter().map(|key| &*key.id).collect::<Vec<_>>()
    );
    let _ = writeln!(w, "queries");
    for query in built.queries.iter() {
        let _ = match query {
            UsageQuery::Worktree { worktree, .. } => {
                writeln!(w, "  worktree {}", name(&worktree.0))
            }
            UsageQuery::Mr { mr } => writeln!(w, "  mr #{}", mr.number),
            other => writeln!(w, "  unexpected {other:?}"),
        };
    }
    out
}

/// The last component of a worktree's root or git directory.
fn name(path: &std::path::Path) -> String {
    let file = path
        .file_name()
        .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    if file == ".git" {
        // The main worktree's key is the common directory.
        path.parent()
            .and_then(|p| p.file_name())
            .map_or(file, |n| format!("{}/.git", n.to_string_lossy()))
    } else {
        file
    }
}

fn head(label: &HeadLabel) -> String {
    match label {
        HeadLabel::Branch(name) => format!("branch:{name}"),
        HeadLabel::Detached(oid) => format!("detached:{oid}"),
        HeadLabel::Unborn(name) => format!("unborn:{name}"),
        HeadLabel::Unknown => "unknown".to_owned(),
    }
}

fn agent(agent: &Agent) -> String {
    match agent {
        Agent::ClaudeCode => "claude-code".to_owned(),
        Agent::Codex => "codex".to_owned(),
        Agent::Other(name) => format!("other:{name}"),
    }
}

fn write_row(w: &mut String, row: &WorktreeRow) {
    let mut flags = Vec::new();
    for (on, flag) in [
        (row.is_active, "current"),
        (row.is_main, "main"),
        (row.locked, "locked"),
        (row.prunable, "prunable"),
    ] {
        if on {
            flags.push(flag);
        }
    }
    let _ = writeln!(
        w,
        "  {} head={} flags=[{}] sessions={} running={}",
        name(&row.key.0),
        head(&row.head),
        flags.join(","),
        row.sessions,
        row.running,
    );
    for session in row.recent.iter() {
        let _ = writeln!(
            w,
            "    session {} {:?} {} ago={}m running={}",
            session.key.id,
            session.title.as_deref().unwrap_or("<untitled>"),
            agent(&session.agent),
            (NOW - session.last_activity.0) / MINUTE,
            session.running,
        );
    }
    let changes = &row.changes;
    let _ = writeln!(
        w,
        "    changes uncommitted={:?} committed={:?} vs_base={:?} counted_ago={}",
        changes.uncommitted,
        changes.committed,
        changes.vs_base,
        changes
            .counted_at
            .map_or("-".to_owned(), |at| format!("{}m", (NOW - at.0) / MINUTE)),
    );
    let _ = match &row.overlaps {
        Overlaps::Pending => writeln!(w, "    overlaps pending"),
        Overlaps::Unknown => writeln!(w, "    overlaps unknown"),
        Overlaps::List(list) if list.is_empty() => writeln!(w, "    overlaps none"),
        Overlaps::List(list) => {
            let _ = writeln!(w, "    overlaps {}", list.len());
            for overlap in list.iter() {
                write_overlap(w, overlap);
            }
            Ok(())
        }
    };
    let _ = match &row.usage {
        RowUsage::Absent => writeln!(w, "    usage absent"),
        RowUsage::Pending => writeln!(w, "    usage pending"),
        RowUsage::NotCounted => writeln!(w, "    usage not-counted"),
        RowUsage::Branch(usage) => writeln!(
            w,
            "    usage branch cumulative={} review={} since={} inherited={} backfilling={}",
            usage.cumulative.total(),
            usage.review.total(),
            usage
                .since
                .map_or("-".to_owned(), |at| format!("{}d", (NOW - at.0) / DAY)),
            usage.may_include_inherited,
            usage.backfilling,
        ),
        RowUsage::Mrs(items) => {
            let _ = writeln!(w, "    usage mrs {}", items.len());
            for item in items.iter() {
                let _ = writeln!(
                    w,
                    "      mr #{} {:?} tokens={:?} shared_with={}",
                    item.label.mr.number,
                    &*item.label.title,
                    item.tokens.map(|t| t.total()),
                    item.shared_with,
                );
            }
            Ok(())
        }
    };
}

fn write_overlap(w: &mut String, overlap: &OverlapRow) {
    let _ = writeln!(
        w,
        "      with {} head={} files={} paths={:?}",
        name(&overlap.other.0),
        head(&overlap.other_head),
        overlap.files,
        overlap.paths,
    );
}
