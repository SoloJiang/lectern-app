//! The reads behind the snapshots: projects and locations, and fact sums by
//! 15-minute bucket (docs/design/usage.md「取整与时间窗口」). Everything is
//! summed in millionths; rounding happens in `snapshot`.

use std::collections::HashMap;
use std::ops::Range;
use std::path::{Path, PathBuf};

use lectern_services::project::ProjectKey;
use lectern_services::session::{Cursor, SessionKey};
use lectern_services::usage::UsageProjectId;
use rusqlite::{Connection, OptionalExtension, params};

use super::ingest::{FileStamp, Place, location, path_bytes};
use super::schema::{LOCATION_DIR, LOCATION_GIT, PROJECT_FOLDER};
use super::{Store, StoreError};

/// A project row with its location.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ProjectRow {
    pub(crate) id: i64,
    pub(crate) pid: UsageProjectId,
    pub(crate) folder: bool,
    /// Opened in Lectern at some point; never-opened repositories count
    /// under 「其他」.
    pub(crate) opened: bool,
    pub(crate) name: Option<String>,
    pub(crate) location: i64,
    pub(crate) path: PathBuf,
}

impl ProjectRow {
    /// The `ProjectKey` this project has while open.
    pub(crate) fn key(&self) -> ProjectKey {
        if self.folder {
            ProjectKey::Folder {
                root: self.path.as_path().into(),
            }
        } else {
            ProjectKey::Git {
                common_dir: self.path.as_path().into(),
            }
        }
    }
}

/// An opened folder project's root, for claiming directory locations.
#[derive(Clone, Debug)]
pub(crate) struct FolderRoot {
    pub(crate) project: i64,
    pub(crate) path: Vec<u8>,
}

/// One group of fact rows, in millionths: input, output, cache read, cache
/// write. `model` is 0 in sums that do not group by model.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Sum {
    pub(crate) location: i64,
    pub(crate) bucket: i64,
    pub(crate) agent: i64,
    pub(crate) model: i64,
    pub(crate) micro: [u64; 4],
}

/// A session's saved reading position.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Saved {
    pub(crate) offset: u64,
    /// The log file when the last batch reached its end; None after any
    /// other batch.
    pub(crate) stamp: Option<FileStamp>,
}

/// Sums and the strings their IDs stand for.
#[derive(Default, Debug)]
pub(crate) struct Totals {
    pub(crate) sums: Vec<Sum>,
    pub(crate) texts: HashMap<i64, Vec<u8>>,
    /// A session counted here has `inheritance_unknown`.
    pub(crate) may_include_inherited: bool,
}

fn os_path(bytes: Vec<u8>) -> PathBuf {
    use std::os::unix::ffi::OsStringExt;
    PathBuf::from(std::ffi::OsString::from_vec(bytes))
}

fn project_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<ProjectRow> {
    let pid: Vec<u8> = r.get(1)?;
    let mut id = [0u8; 16];
    if pid.len() == 16 {
        id.copy_from_slice(&pid);
    }
    Ok(ProjectRow {
        id: r.get(0)?,
        pid: UsageProjectId(id),
        folder: r.get::<_, i64>(2)? == PROJECT_FOLDER,
        opened: r.get(3)?,
        name: r.get(4)?,
        location: r.get(5)?,
        path: os_path(r.get(6)?),
    })
}

const PROJECT_SELECT: &str = "SELECT p.id, p.pid, p.kind, p.opened, p.name, p.location, l.path
     FROM project p JOIN location l ON l.id = p.location";

/// Is `path` the same as `root` or inside it (by whole components)?
pub(crate) fn under(path: &[u8], root: &[u8]) -> bool {
    if root.is_empty() {
        return false;
    }
    path == root
        || (path.starts_with(root) && (root.ends_with(b"/") || path.get(root.len()) == Some(&b'/')))
}

impl Store {
    pub(crate) fn projects(&self) -> Result<Vec<ProjectRow>, StoreError> {
        self.read(|conn| {
            conn.prepare_cached(&format!("{PROJECT_SELECT} ORDER BY p.id"))?
                .query_map([], project_row)?
                .collect()
        })
    }

    /// The saved positions of these sessions (those the database knows).
    /// Many keys read the whole table once, a few look each one up.
    pub(crate) fn saved(
        &self,
        keys: &[SessionKey],
    ) -> Result<HashMap<SessionKey, Saved>, StoreError> {
        fn saved_of(cursor: &[u8], stamp: Option<&[u8]>) -> Saved {
            Saved {
                offset: Cursor::from_bytes(cursor).map_or(0, |c| c.offset()),
                stamp: stamp.and_then(FileStamp::from_bytes),
            }
        }
        const SELECT: &str = "SELECT a.text, b.text, s.cursor, s.end_stamp FROM session s
             JOIN intern a ON a.id = s.source JOIN intern b ON b.id = s.sid";
        self.read(|conn| {
            let mut out = HashMap::with_capacity(keys.len());
            if keys.len() <= 64 {
                let mut stmt =
                    conn.prepare_cached(&format!("{SELECT} WHERE a.text = ?1 AND b.text = ?2"))?;
                for key in keys {
                    let row: Option<(Vec<u8>, Option<Vec<u8>>)> = stmt
                        .query_row(params![key.source.0.as_bytes(), key.id.as_bytes()], |r| {
                            Ok((r.get(2)?, r.get(3)?))
                        })
                        .optional()?;
                    if let Some((cursor, stamp)) = row {
                        out.insert(key.clone(), saved_of(&cursor, stamp.as_deref()));
                    }
                }
                return Ok(out);
            }
            let wanted: HashMap<(&[u8], &[u8]), &SessionKey> = keys
                .iter()
                .map(|k| ((k.source.0.as_bytes(), k.id.as_bytes()), k))
                .collect();
            let mut stmt = conn.prepare_cached(SELECT)?;
            let mut rows = stmt.query([])?;
            while let Some(r) = rows.next()? {
                let (source, sid): (Vec<u8>, Vec<u8>) = (r.get(0)?, r.get(1)?);
                if let Some(key) = wanted.get(&(source.as_slice(), sid.as_slice())) {
                    let cursor: Vec<u8> = r.get(2)?;
                    let stamp: Option<Vec<u8>> = r.get(3)?;
                    out.insert((*key).clone(), saved_of(&cursor, stamp.as_deref()));
                }
            }
            Ok(out)
        })
    }

    /// Directory locations: (ID, path bytes).
    pub(crate) fn dir_locations(&self) -> Result<Vec<(i64, Vec<u8>)>, StoreError> {
        self.read(|conn| {
            conn.prepare_cached("SELECT id, path FROM location WHERE kind = ?1 ORDER BY id")?
                .query_map(params![LOCATION_DIR], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect()
        })
    }

    /// The project an open `ProjectKey` stands for, if the database has it.
    pub(crate) fn project_by_key(
        &self,
        key: &ProjectKey,
    ) -> Result<Option<ProjectRow>, StoreError> {
        let (kind, path) = key_parts(key);
        self.read(|conn| {
            conn.prepare_cached(&format!(
                "{PROJECT_SELECT} WHERE l.kind = ?1 AND l.path = ?2"
            ))?
            .query_row(params![kind, path_bytes(path)], project_row)
            .optional()
        })
    }

    pub(crate) fn project_by_id(
        &self,
        pid: &UsageProjectId,
    ) -> Result<Option<ProjectRow>, StoreError> {
        self.read(|conn| {
            conn.prepare_cached(&format!("{PROJECT_SELECT} WHERE p.pid = ?1"))?
                .query_row(params![&pid.0[..]], project_row)
                .optional()
        })
    }

    /// Records an opened project: its location and project, marked opened,
    /// with its display name. A folder project is created here; a
    /// repository's project may exist already from its sessions.
    pub(crate) fn project_opened(
        &self,
        key: &ProjectKey,
        name: &str,
    ) -> Result<ProjectRow, StoreError> {
        self.transaction(|tx| {
            let id = match key {
                ProjectKey::Git { common_dir } => {
                    let loc = location(tx, &Place::Git(common_dir.to_path_buf()))?;
                    project_of(tx, loc)?
                }
                ProjectKey::Folder { root } => {
                    let loc = location(tx, &Place::Dir(root.to_path_buf()))?;
                    tx.prepare_cached(
                        "INSERT OR IGNORE INTO project (pid, kind, location)
                         VALUES (randomblob(16), ?1, ?2)",
                    )?
                    .execute(params![PROJECT_FOLDER, loc])?;
                    project_of(tx, loc)?
                }
            };
            tx.prepare_cached("UPDATE project SET opened = 1, name = ?2 WHERE id = ?1")?
                .execute(params![id, name])?;
            tx.prepare_cached(&format!("{PROJECT_SELECT} WHERE p.id = ?1"))?
                .query_row(params![id], project_row)
        })
    }

    /// Sums of one location's facts in `range`, by bucket, agent and model.
    pub(crate) fn sums_at(
        &self,
        locations: &[i64],
        range: Range<i64>,
    ) -> Result<Totals, StoreError> {
        self.read(|conn| {
            let mut totals = Totals::default();
            let mut stmt = conn.prepare_cached(
                "SELECT bucket, agent, model, sum(input), sum(output), sum(cache_read), sum(cache_write)
                 FROM fact WHERE location = ?1 AND at >= ?2 AND at < ?3
                 GROUP BY bucket, agent, model",
            )?;
            for &location in locations {
                let rows = stmt.query_map(params![location, range.start, range.end], |r| {
                    Ok(Sum {
                        location,
                        bucket: r.get(0)?,
                        agent: r.get(1)?,
                        model: r.get(2)?,
                        micro: micro(r, 3)?,
                    })
                })?;
                for row in rows {
                    totals.sums.push(row?);
                }
            }
            totals.may_include_inherited = inherited_unknown(conn, Some(locations), &range)?;
            fill_texts(conn, &mut totals)?;
            Ok(totals)
        })
    }

    /// Sums of every fact in `range`, by location, bucket and agent.
    pub(crate) fn sums_all(&self, range: Range<i64>) -> Result<Totals, StoreError> {
        self.read(|conn| {
            let mut totals = Totals::default();
            let rows: Vec<Sum> = conn
                .prepare_cached(
                    "SELECT location, bucket, agent, sum(input), sum(output), sum(cache_read), sum(cache_write)
                     FROM fact WHERE at >= ?1 AND at < ?2
                     GROUP BY location, bucket, agent",
                )?
                .query_map(params![range.start, range.end], |r| {
                    Ok(Sum {
                        location: r.get(0)?,
                        bucket: r.get(1)?,
                        agent: r.get(2)?,
                        model: 0,
                        micro: micro(r, 3)?,
                    })
                })?
                .collect::<rusqlite::Result<_>>()?;
            totals.sums = rows;
            totals.may_include_inherited = inherited_unknown(conn, None, &range)?;
            fill_texts(conn, &mut totals)?;
            Ok(totals)
        })
    }
}

fn key_parts(key: &ProjectKey) -> (i64, &Path) {
    match key {
        ProjectKey::Git { common_dir } => (LOCATION_GIT, common_dir),
        ProjectKey::Folder { root } => (LOCATION_DIR, root),
    }
}

fn project_of(conn: &Connection, location: i64) -> rusqlite::Result<i64> {
    conn.prepare_cached("SELECT id FROM project WHERE location = ?1")?
        .query_row(params![location], |r| r.get(0))
}

fn micro(r: &rusqlite::Row<'_>, from: usize) -> rusqlite::Result<[u64; 4]> {
    let mut out = [0u64; 4];
    for (i, slot) in out.iter_mut().enumerate() {
        *slot = r.get::<_, Option<i64>>(from + i)?.unwrap_or(0).max(0) as u64;
    }
    Ok(out)
}

fn inherited_unknown(
    conn: &Connection,
    locations: Option<&[i64]>,
    range: &Range<i64>,
) -> rusqlite::Result<bool> {
    let mut stmt = conn.prepare_cached(
        "SELECT EXISTS (
           SELECT 1 FROM fact f JOIN session s ON s.id = f.session
           WHERE s.inheritance_unknown = 1 AND f.at >= ?1 AND f.at < ?2
             AND (?3 IS NULL OR f.location = ?3))",
    )?;
    match locations {
        None => stmt.query_row(params![range.start, range.end, None::<i64>], |r| r.get(0)),
        Some(locations) => {
            for &l in locations {
                if stmt.query_row(params![range.start, range.end, l], |r| r.get::<_, bool>(0))? {
                    return Ok(true);
                }
            }
            Ok(false)
        }
    }
}

fn fill_texts(conn: &Connection, totals: &mut Totals) -> rusqlite::Result<()> {
    let mut stmt = conn.prepare_cached("SELECT text FROM intern WHERE id = ?1")?;
    for s in &totals.sums {
        for id in [s.agent, s.model] {
            if id != 0 && !totals.texts.contains_key(&id) {
                let text: Option<Vec<u8>> = stmt.query_row(params![id], |r| r.get(0)).optional()?;
                totals.texts.insert(id, text.unwrap_or_default());
            }
        }
    }
    Ok(())
}
