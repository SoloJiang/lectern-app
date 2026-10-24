//! A snapshot as text, for the insta snapshots of the `snapshot_*` tests: the
//! whole model, one line per row, with no addresses or paths that change.

use std::fmt::Write;

use lectern_services::session::Agent;

use super::{Card, GroupKind, HomeSnapshot, OutsideRow, SessionRow, SidebarRow, Signals};
use crate::testing::{MINUTE, NOW};

pub(crate) fn render(snapshot: &HomeSnapshot) -> String {
    let mut out = String::new();
    let w = &mut out;
    let _ = writeln!(
        w,
        "project {:?} branch={} settled={}",
        &*snapshot.name,
        snapshot.branch.as_deref().unwrap_or("-"),
        snapshot.files_settled,
    );
    let _ = writeln!(
        w,
        "sources project={} sessions={:?} overview={:?} usage={:?}",
        snapshot.sources.project,
        snapshot
            .sources
            .sessions
            .iter()
            .map(|(id, generation)| format!("{}@{generation}", id.0))
            .collect::<Vec<_>>(),
        snapshot.sources.overview,
        snapshot.sources.usage,
    );
    let sidebar = &snapshot.sidebar;
    let _ = writeln!(
        w,
        "sidebar groups={} session_rows={} packages={} counts={}",
        sidebar.groups.len(),
        sidebar.session_rows,
        sidebar.packages.len(),
        sidebar.package_counts,
    );
    for group in sidebar.groups.iter() {
        let kind = match &group.kind {
            GroupKind::Worktree { branch, is_active } => {
                format!("worktree {branch} active={is_active}")
            }
            GroupKind::Folder => "folder".to_owned(),
            GroupKind::Other => "other".to_owned(),
        };
        let _ = writeln!(w, "  group {kind}");
        for session in group.sessions.iter() {
            let _ = writeln!(w, "    {}", session_line(session));
        }
    }
    for package in sidebar.packages.iter() {
        let _ = writeln!(
            w,
            "  package {:?} name={} depth={} today={}",
            &*package.dir, package.name, package.depth, package.changed_today,
        );
    }
    let _ = writeln!(w, "rows");
    for row in sidebar.rows.iter() {
        let _ = match row {
            SidebarRow::SessionsHeader => writeln!(w, "  sessions-header"),
            SidebarRow::WorktreeHeader(g) => writeln!(w, "  worktree-header {g}"),
            SidebarRow::Session { group, index } => writeln!(w, "  session {group}.{index}"),
            SidebarRow::SessionsEmpty => writeln!(w, "  sessions-empty"),
            SidebarRow::PackagesHeader => writeln!(w, "  packages-header"),
            SidebarRow::Package(i) => writeln!(w, "  package {i}"),
            SidebarRow::PackagesSkeleton => writeln!(w, "  packages-skeleton"),
        };
    }
    let main = &snapshot.main;
    let _ = writeln!(
        w,
        "main overview_ready={} needs_review={} done={}",
        main.overview_ready,
        main.needs_review.len(),
        main.done.len(),
    );
    for card in main.needs_review.iter() {
        let _ = writeln!(w, "  needs-review {}", card_line(card));
    }
    let _ = match main.outside {
        OutsideRow::Hidden => writeln!(w, "  outside hidden"),
        OutsideRow::Pending => writeln!(w, "  outside pending"),
        OutsideRow::Files(n) => writeln!(w, "  outside files={n}"),
        OutsideRow::Capped(n) => writeln!(w, "  outside capped={n}"),
    };
    for card in main.done.iter() {
        let _ = writeln!(w, "  done {}", card_line(card));
    }
    match &snapshot.usage {
        None => {
            let _ = writeln!(w, "usage none");
        }
        Some(usage) => {
            let _ = writeln!(
                w,
                "usage tokens_7d={:?} tightest={:?}",
                usage.tokens_7d,
                usage.tightest.as_ref().map(|t| (
                    agent(&t.agent),
                    t.used_percent,
                    t.resets_at.map(|r| r.0)
                )),
            );
        }
    }
    out
}

fn agent(agent: &Agent) -> String {
    match agent {
        Agent::ClaudeCode => "claude-code".to_owned(),
        Agent::Codex => "codex".to_owned(),
        Agent::Other(name) => format!("other:{name}"),
    }
}

fn session_line(row: &SessionRow) -> String {
    format!(
        "{} {:?} agent={} ago={}m running={} pending={}",
        row.key.id,
        row.title.as_deref().unwrap_or("<untitled>"),
        agent(&row.agent),
        (NOW - row.last_activity.0) / MINUTE,
        row.running,
        row.tail_pending,
    )
}

fn card_line(card: &Card) -> String {
    let Signals {
        drifted,
        critical,
        changed_since_seen,
    } = card.signals;
    let progress = card
        .progress
        .map_or("-".to_owned(), |p| format!("{}/{}", p.seen, p.total));
    format!(
        "{} progress={progress} drifted={drifted} critical={critical} changed={changed_since_seen}",
        session_line(&card.session),
    )
}
