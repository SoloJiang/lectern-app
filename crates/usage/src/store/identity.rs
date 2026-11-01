//! Project identity in the database (docs/design/usage.md「项目标识」): the
//! facts a repository showed each time it was identified (`project_fact`),
//! the appended alias records (`alias`) and what replaying them gives, and
//! the user's commands.
//!
//! Observations are never rewritten: a project that turns out to be another
//! one is joined to it by an alias, and an undo is a record of its own that
//! cancels the alias it points at. The mapping from a project to the one it
//! finally is comes from replaying the records in order, so undoing a merge
//! brings the usage back without touching any other row.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use lectern_services::usage::{ForgeRepo, TemporaryReason, UsageCommand, UsageProjectId};
use rusqlite::{Connection, OptionalExtension, Transaction, params};

use super::ingest::{Place, location};
use super::query::{ProjectRow, Temp, project_by_id_on, projects_on, reason_code};
use super::schema::PROJECT_GIT;
use super::{Store, StoreError};
use crate::identity::{Identification, KnownProject, Previous, RepoFacts, identify};

/// What an alias record says. The codes are stored.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AliasKind {
    /// `src` (the project made at a repository's new path) is the repository
    /// that was at `dst`'s path.
    Move,
    /// `src` (made after the history was rewritten) is `dst`.
    Rewrite,
    /// The user: `src`, whose path is gone, joins `dst`.
    Merge,
    /// The user: the temporary `src` is `dst`.
    ConfirmSame,
    /// The user: the temporary `src` is a project of its own.
    ConfirmNew,
    /// `src`, a project without a commit when it was made, has one now and
    /// is a project of its own.
    Promote,
    /// Cancels the record `undoes`.
    Undo,
}

impl AliasKind {
    fn code(self) -> i64 {
        match self {
            AliasKind::Move => 0,
            AliasKind::Rewrite => 1,
            AliasKind::Merge => 2,
            AliasKind::ConfirmSame => 3,
            AliasKind::ConfirmNew => 4,
            AliasKind::Promote => 5,
            AliasKind::Undo => 6,
        }
    }

    fn of(code: i64) -> Option<AliasKind> {
        Some(match code {
            0 => AliasKind::Move,
            1 => AliasKind::Rewrite,
            2 => AliasKind::Merge,
            3 => AliasKind::ConfirmSame,
            4 => AliasKind::ConfirmNew,
            5 => AliasKind::Promote,
            6 => AliasKind::Undo,
            _ => return None,
        })
    }

    /// The record joins `src` to `dst`.
    fn joins(self) -> bool {
        matches!(
            self,
            AliasKind::Move | AliasKind::Rewrite | AliasKind::Merge | AliasKind::ConfirmSame
        )
    }

    /// The user made the record (the Usage page lists it and can undo it).
    pub(crate) fn by_user(self) -> bool {
        matches!(
            self,
            AliasKind::Merge | AliasKind::ConfirmSame | AliasKind::ConfirmNew
        )
    }

    /// A fixed name for the log.
    pub(crate) fn name(self) -> &'static str {
        match self {
            AliasKind::Move => "move",
            AliasKind::Rewrite => "rewrite",
            AliasKind::Merge => "merge",
            AliasKind::ConfirmSame => "confirm_same",
            AliasKind::ConfirmNew => "confirm_new",
            AliasKind::Promote => "promote",
            AliasKind::Undo => "undo",
        }
    }
}

/// One stored alias record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct AliasRow {
    pub(crate) id: i64,
    pub(crate) kind: AliasKind,
    pub(crate) src: i64,
    pub(crate) dst: Option<i64>,
    pub(crate) undoes: Option<i64>,
    pub(crate) at: i64,
}

/// The alias records replayed in order: the records an undo cancelled are
/// out, and of the records that join the same project the last one counts.
#[derive(Clone, Debug, Default)]
pub(crate) struct Aliases {
    /// The records in force, by ID.
    active: HashMap<i64, AliasRow>,
    /// `src` to the record that joins it.
    joined: HashMap<i64, AliasRow>,
    confirmed: HashMap<i64, i64>,
    promoted: HashSet<i64>,
}

impl Aliases {
    pub(crate) fn replay(rows: &[AliasRow]) -> Aliases {
        let mut out = Aliases::default();
        let mut sorted: Vec<&AliasRow> = rows.iter().collect();
        sorted.sort_by_key(|r| r.id);
        let mut active: Vec<&AliasRow> = Vec::new();
        for row in sorted {
            if row.kind == AliasKind::Undo {
                if let Some(target) = row.undoes {
                    active.retain(|r| r.id != target);
                }
            } else {
                active.push(row);
            }
        }
        for row in active {
            out.active.insert(row.id, *row);
            if row.kind.joins() && row.dst.is_some() {
                out.joined.insert(row.src, *row);
            }
            match row.kind {
                AliasKind::ConfirmNew => {
                    out.confirmed.insert(row.src, row.id);
                }
                AliasKind::Promote => {
                    out.promoted.insert(row.src);
                }
                _ => {}
            }
        }
        out
    }

    /// The record that joins `project` to another, if any.
    pub(crate) fn joined(&self, project: i64) -> Option<&AliasRow> {
        self.joined.get(&project)
    }

    /// The project `project` finally is: follows the records that join
    /// projects. A loop (which a command never creates) stops where it
    /// closes.
    pub(crate) fn final_of(&self, project: i64) -> i64 {
        let mut at = project;
        let mut seen = HashSet::new();
        while seen.insert(at) {
            match self.joined.get(&at).and_then(|r| r.dst) {
                Some(next) => at = next,
                None => break,
            }
        }
        at
    }

    /// The record of the user's "it is a new project" for `project`.
    pub(crate) fn confirmed(&self, project: i64) -> Option<i64> {
        self.confirmed.get(&project).copied()
    }

    /// A temporary project that was confirmed or promoted is not temporary
    /// any more.
    pub(crate) fn settled(&self, project: i64) -> bool {
        self.confirmed.contains_key(&project) || self.promoted.contains(&project)
    }

    /// The records in force that the user made and that join a project, the
    /// newest first.
    pub(crate) fn user_joins(&self) -> Vec<AliasRow> {
        let mut out: Vec<AliasRow> = self
            .joined
            .values()
            .filter(|r| r.kind.by_user())
            .copied()
            .collect();
        out.sort_by_key(|r| std::cmp::Reverse(r.id));
        out
    }

    pub(crate) fn get(&self, id: i64) -> Option<&AliasRow> {
        self.active.get(&id)
    }
}

pub(crate) fn aliases_on(conn: &Connection) -> rusqlite::Result<Vec<AliasRow>> {
    let mut stmt =
        conn.prepare_cached("SELECT id, kind, src, dst, undoes, at FROM alias ORDER BY id")?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, i64>(1)?,
            AliasRow {
                id: r.get(0)?,
                kind: AliasKind::Undo,
                src: r.get(2)?,
                dst: r.get(3)?,
                undoes: r.get(4)?,
                at: r.get(5)?,
            },
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (code, mut row) = row?;
        // A record of a kind this version does not know (a newer one wrote
        // it) is skipped.
        if let Some(kind) = AliasKind::of(code) {
            row.kind = kind;
            out.push(row);
        }
    }
    Ok(out)
}

fn insert_alias(
    conn: &Connection,
    kind: AliasKind,
    src: i64,
    dst: Option<i64>,
    undoes: Option<i64>,
    at: i64,
) -> rusqlite::Result<i64> {
    conn.prepare_cached(
        "INSERT INTO alias (kind, src, dst, undoes, at) VALUES (?1, ?2, ?3, ?4, ?5)
         RETURNING id",
    )?
    .query_row(params![kind.code(), src, dst, undoes, at], |r| r.get(0))
}

fn join<'a>(items: impl IntoIterator<Item = &'a String>, sep: &str) -> String {
    items
        .into_iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(sep)
}

fn split(text: &str, sep: char) -> impl Iterator<Item = String> + '_ {
    text.split(sep).filter(|s| !s.is_empty()).map(str::to_owned)
}

/// The latest identification of every project that has one.
fn latest_facts(conn: &Connection) -> rusqlite::Result<HashMap<i64, RepoFacts>> {
    let mut stmt = conn.prepare_cached(
        "SELECT project, roots, remotes, primary_name, primary_addr, forge_host, forge_id, unborn,
                tip
         FROM project_fact WHERE id IN (SELECT max(id) FROM project_fact GROUP BY project)",
    )?;
    let rows = stmt.query_map([], |r| {
        let host: Option<String> = r.get(5)?;
        let id: Option<String> = r.get(6)?;
        let roots: String = r.get(1)?;
        let remotes: String = r.get(2)?;
        Ok((
            r.get::<_, i64>(0)?,
            RepoFacts {
                roots: split(&roots, ' ').collect(),
                remotes: split(&remotes, '\n').collect(),
                primary_name: r.get(3)?,
                primary: r.get(4)?,
                forge: host.zip(id).map(|(host, id)| ForgeRepo {
                    host: host.into(),
                    id: id.into(),
                }),
                unborn: r.get(7)?,
                tip: r.get(8)?,
            },
        ))
    })?;
    rows.collect()
}

fn insert_fact(
    conn: &Connection,
    project: i64,
    location: i64,
    at: i64,
    facts: &RepoFacts,
) -> rusqlite::Result<()> {
    conn.prepare_cached(
        "INSERT INTO project_fact
           (project, location, at, roots, remotes, primary_name, primary_addr, forge_host,
            forge_id, unborn, tip)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
    )?
    .execute(params![
        project,
        location,
        at,
        join(&facts.roots, " "),
        join(&facts.remotes, "\n"),
        facts.primary_name,
        facts.primary,
        facts.forge.as_ref().map(|f| &*f.host),
        facts.forge.as_ref().map(|f| &*f.id),
        facts.unborn,
        facts.tip,
    ])?;
    Ok(())
}

/// What recording an identification did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// The project recorded here.
    Same,
    /// The repository continues a project whose path is gone.
    Moved,
    /// History was rewritten; joined to the recorded project.
    Rewritten,
    /// A project of its own.
    New,
    /// A new project for now.
    Temporary(TemporaryReason),
}

impl Outcome {
    /// A fixed name for the log.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Outcome::Same => "same",
            Outcome::Moved => "moved",
            Outcome::Rewritten => "rewritten",
            Outcome::New => "new",
            Outcome::Temporary(TemporaryReason::LocalRewrite) => "temporary_local_rewrite",
            Outcome::Temporary(TemporaryReason::PrimaryRemoteChanged) => {
                "temporary_primary_remote_changed"
            }
            Outcome::Temporary(TemporaryReason::ForgeIdPending) => "temporary_forge_id_pending",
            Outcome::Temporary(TemporaryReason::Unborn) => "temporary_unborn",
        }
    }
}

/// A recorded identification.
#[derive(Clone, Debug)]
pub(crate) struct Recorded {
    /// The project the opened repository is now.
    pub(crate) project: ProjectRow,
    pub(crate) outcome: Outcome,
}

/// Whether the group of `final_id`, the projects that finally are it, still
/// has a repository at one of its paths.
fn present(
    final_id: i64,
    members: &HashMap<i64, Vec<&ProjectRow>>,
    latest_at: &HashMap<i64, i64>,
    gone: &HashSet<i64>,
) -> bool {
    members.get(&final_id).is_some_and(|group| {
        group
            .iter()
            .any(|m| !gone.contains(&m.location) && latest_at.get(&m.location) == Some(&m.id))
    })
}

impl Store {
    /// What the last identification of the repository at `common_dir`
    /// found: the primary remote chosen, and the tip the roots were found
    /// from with the roots.
    pub(crate) fn previous(&self, common_dir: &Path) -> Result<Previous, StoreError> {
        self.read(|conn| {
            let row: Option<(Option<String>, Option<String>, String)> = conn
                .prepare_cached(
                    "SELECT f.primary_name, f.tip, f.roots FROM project_fact f
                     JOIN location l ON l.id = f.location
                     WHERE l.kind = ?1 AND l.path = ?2 ORDER BY f.id DESC LIMIT 1",
                )?
                .query_row(
                    params![super::schema::LOCATION_GIT, super::path_bytes(common_dir)],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .optional()?;
            Ok(
                row.map_or_else(Previous::default, |(primary_name, tip, roots)| Previous {
                    primary_name,
                    tip,
                    roots: split(&roots, ' ').collect(),
                }),
            )
        })
    }

    /// Whether the project that owns the repository's location is a
    /// temporary one for lack of a commit.
    pub(crate) fn is_unborn(&self, common_dir: &Path) -> Result<bool, StoreError> {
        self.read(|conn| {
            let row: Option<(Option<i64>, i64)> = conn
                .prepare_cached(
                    "SELECT p.reason, p.id FROM project p JOIN location l ON l.id = p.location
                     WHERE l.kind = ?1 AND l.path = ?2 ORDER BY p.since DESC, p.id DESC LIMIT 1",
                )?
                .query_row(
                    params![super::schema::LOCATION_GIT, super::path_bytes(common_dir)],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            let Some((Some(reason), id)) = row else {
                return Ok(false);
            };
            if reason != reason_code(TemporaryReason::Unborn) {
                return Ok(false);
            }
            let aliases = Aliases::replay(&aliases_on(conn)?);
            Ok(!aliases.settled(id) && aliases.joined(id).is_none())
        })
    }

    /// Identifies the repository at `common_dir` as it shows now (`facts`)
    /// and records the result in one transaction. `gone` is the set of
    /// location IDs whose path no longer holds a repository (the caller
    /// looked, outside the transaction).
    pub(crate) fn identify_open(
        &self,
        common_dir: &Path,
        name: &str,
        facts: &RepoFacts,
        now_ms: i64,
        gone: &HashSet<i64>,
    ) -> Result<Recorded, StoreError> {
        self.transaction(|tx| {
            let loc = location(tx, &Place::Git(common_dir.to_path_buf()))?;
            record(tx, loc, name, facts, now_ms, gone)
        })
    }

    /// The records in force, for the snapshots.
    pub(crate) fn aliases(&self) -> Result<Aliases, StoreError> {
        self.read(|conn| aliases_on(conn).map(|rows| Aliases::replay(&rows)))
    }

    /// Applies a command of the Usage page.
    pub(crate) fn command(
        &self,
        command: &UsageCommand,
        now_ms: i64,
    ) -> Result<Commanded, StoreError> {
        self.transaction(|tx| run_command(tx, command, now_ms))
    }
}

fn record(
    tx: &Transaction<'_>,
    loc: i64,
    name: &str,
    facts: &RepoFacts,
    now_ms: i64,
    gone: &HashSet<i64>,
) -> rusqlite::Result<Recorded> {
    let projects = projects_on(tx)?;
    let aliases = Aliases::replay(&aliases_on(tx)?);
    let latest = latest_facts(tx)?;

    // The projects at this location, the latest first.
    let mut at_loc: Vec<&ProjectRow> = projects.iter().filter(|p| p.location == loc).collect();
    at_loc.sort_by_key(|p| std::cmp::Reverse((p.since, p.id)));
    let here = at_loc.iter().find(|p| latest.contains_key(&p.id)).copied();
    // The project made when the location first appeared and never
    // identified: the repository's own project when nothing contradicts it.
    let blank = at_loc.iter().find(|p| !latest.contains_key(&p.id)).copied();

    let mut latest_at: HashMap<i64, i64> = HashMap::new();
    let mut ordered: Vec<&ProjectRow> = projects.iter().collect();
    ordered.sort_by_key(|p| (p.since, p.id));
    for p in &ordered {
        latest_at.insert(p.location, p.id);
    }
    let mut members: HashMap<i64, Vec<&ProjectRow>> = HashMap::new();
    for p in &projects {
        members.entry(aliases.final_of(p.id)).or_default().push(p);
    }
    let mut known = Vec::new();
    for p in &projects {
        let identified = latest.get(&p.id);
        let is_here = here.is_some_and(|h| h.id == p.id);
        if is_here {
            if let Some(facts) = identified {
                known.push(KnownProject {
                    id: UsageProjectId(pid_of(p)),
                    here: true,
                    path_missing: false,
                    facts: facts.clone(),
                });
            }
            continue;
        }
        let Some(candidate_facts) = identified else {
            continue;
        };
        if p.folder || aliases.joined(p.id).is_some() {
            continue;
        }
        if !present(p.id, &members, &latest_at, gone) {
            known.push(KnownProject {
                id: UsageProjectId(pid_of(p)),
                here: false,
                path_missing: true,
                facts: candidate_facts.clone(),
            });
        }
    }

    let result = identify(&known, facts);
    let by_pid = |pid: UsageProjectId| projects.iter().find(|p| pid_of(p) == pid.0);
    let id_of = |pid: UsageProjectId| by_pid(pid).map(|p| p.id);

    // The existing project the repository can take over: the placeholder of
    // a location nothing has identified, or the one that had no commit.
    let unborn_here = here.filter(|h| latest.get(&h.id).is_some_and(|f| f.unborn));
    let takeover = unborn_here
        .or(blank.filter(|_| here.is_none()))
        .map(|p| p.id);

    let (target, outcome) = match result {
        Identification::Same(pid) if id_of(pid).is_some() => {
            (id_of(pid).unwrap_or_default(), Outcome::Same)
        }
        Identification::Moved { from } if id_of(from).is_some() => {
            let id = match takeover {
                Some(id) => id,
                None => new_project(tx, loc, name, now_ms)?,
            };
            insert_alias(tx, AliasKind::Move, id, id_of(from), None, now_ms)?;
            (id, Outcome::Moved)
        }
        Identification::Rewritten { project } if id_of(project).is_some() => {
            let id = new_project(tx, loc, name, now_ms)?;
            insert_alias(tx, AliasKind::Rewrite, id, id_of(project), None, now_ms)?;
            (id, Outcome::Rewritten)
        }
        // `identify` only names projects it was given: the guards above
        // keep a result that does not match one from joining nothing.
        Identification::New
        | Identification::Same(_)
        | Identification::Moved { .. }
        | Identification::Rewritten { .. } => {
            let id = match takeover {
                Some(id) => id,
                None => new_project(tx, loc, name, now_ms)?,
            };
            if unborn_here.is_some_and(|h| h.id == id) {
                insert_alias(tx, AliasKind::Promote, id, None, None, now_ms)?;
            }
            (id, Outcome::New)
        }
        Identification::Temporary { reason, candidate } => {
            let id = match blank {
                Some(p) if here.is_none() => p.id,
                _ => new_project(tx, loc, name, now_ms)?,
            };
            tx.prepare_cached("UPDATE project SET reason = ?2, candidate = ?3 WHERE id = ?1")?
                .execute(params![id, reason_code(reason), candidate.and_then(id_of)])?;
            (id, Outcome::Temporary(reason))
        }
    };

    tx.prepare_cached(
        "UPDATE project SET opened = 1, name = CASE WHEN ?2 = '' THEN name ELSE ?2 END
         WHERE id = ?1",
    )?
    .execute(params![target, name])?;
    if latest.get(&target) != Some(facts) {
        insert_fact(tx, target, loc, now_ms, facts)?;
    }
    let project = project_by_id_on(tx, target)?.ok_or(rusqlite::Error::QueryReturnedNoRows)?;
    Ok(Recorded { project, outcome })
}

fn pid_of(p: &ProjectRow) -> [u8; 16] {
    p.pid.0
}

fn new_project(tx: &Transaction<'_>, loc: i64, name: &str, now_ms: i64) -> rusqlite::Result<i64> {
    tx.prepare_cached(
        "INSERT INTO project (pid, kind, location, opened, name, since)
         VALUES (randomblob(16), ?1, ?2, 1, ?3, ?4) RETURNING id",
    )?
    .query_row(params![PROJECT_GIT, loc, name, now_ms], |r| r.get(0))
}

/// What a command did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Commanded {
    Applied(AliasKind),
    Rejected(&'static str),
}

fn run_command(
    tx: &Transaction<'_>,
    command: &UsageCommand,
    now_ms: i64,
) -> rusqlite::Result<Commanded> {
    let projects = projects_on(tx)?;
    let aliases = Aliases::replay(&aliases_on(tx)?);
    let find = |pid: &UsageProjectId| projects.iter().find(|p| p.pid == *pid);
    let temporary = |p: &ProjectRow| {
        p.temp.is_some() && !aliases.settled(p.id) && aliases.joined(p.id).is_none()
    };
    // Joining `src` to `dst` must not close a loop.
    let closes_loop = |src: i64, dst: i64| aliases.final_of(dst) == src;
    match command {
        UsageCommand::Merge { from, into } => {
            let (Some(src), Some(dst)) = (find(from), find(into)) else {
                return Ok(Commanded::Rejected("unknown_project"));
            };
            if src.id == dst.id || closes_loop(src.id, dst.id) {
                return Ok(Commanded::Rejected("loop"));
            }
            if aliases.joined(src.id).is_some() {
                return Ok(Commanded::Rejected("already_merged"));
            }
            insert_alias(tx, AliasKind::Merge, src.id, Some(dst.id), None, now_ms)?;
            Ok(Commanded::Applied(AliasKind::Merge))
        }
        UsageCommand::ConfirmSame { temporary: t, into } => {
            let (Some(src), Some(dst)) = (find(t), find(into)) else {
                return Ok(Commanded::Rejected("unknown_project"));
            };
            if !temporary(src) {
                return Ok(Commanded::Rejected("not_temporary"));
            }
            if src.id == dst.id || closes_loop(src.id, dst.id) {
                return Ok(Commanded::Rejected("loop"));
            }
            insert_alias(
                tx,
                AliasKind::ConfirmSame,
                src.id,
                Some(dst.id),
                None,
                now_ms,
            )?;
            Ok(Commanded::Applied(AliasKind::ConfirmSame))
        }
        UsageCommand::ConfirmNew { temporary: t } => {
            let Some(src) = find(t) else {
                return Ok(Commanded::Rejected("unknown_project"));
            };
            if !temporary(src) {
                return Ok(Commanded::Rejected("not_temporary"));
            }
            insert_alias(tx, AliasKind::ConfirmNew, src.id, None, None, now_ms)?;
            Ok(Commanded::Applied(AliasKind::ConfirmNew))
        }
        UsageCommand::Undo { alias } => {
            let Some(target) = aliases.get(i64::try_from(alias.0).unwrap_or(-1)) else {
                return Ok(Commanded::Rejected("unknown_alias"));
            };
            if !target.kind.by_user() {
                return Ok(Commanded::Rejected("not_undoable"));
            }
            insert_alias(
                tx,
                AliasKind::Undo,
                target.src,
                None,
                Some(target.id),
                now_ms,
            )?;
            Ok(Commanded::Applied(AliasKind::Undo))
        }
    }
}

/// The temporary project of a row, for the snapshots.
pub(crate) fn temporary_of(row: &ProjectRow, aliases: &Aliases) -> Option<Temp> {
    row.temp
        .filter(|_| !aliases.settled(row.id) && aliases.joined(row.id).is_none())
}
