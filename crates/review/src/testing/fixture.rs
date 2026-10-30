//! The fixtures of the `review/open_session_*`, `review/cutoffs_cold`,
//! `review/rows_first`, `review/attribute_session_50` and
//! `review/mark_seen_5000` benches (docs/design/review.md「性能」「每天的
//! 仓库规模」): synthetic repositories from `TestRepo`, sessions from
//! `FakeSessions`.

use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use serde_json::{Value, json};

use lectern_cache::{Budgets, Cache, Config as CacheConfig};
use lectern_git::testing::{IndexEntry, TestHead, TestRepo};
use lectern_git::{Mode, Oid};
use lectern_services::changes::{
    ChangeDetail, ChangePath, ChangeStatus, DiffHunk, FileChange, FileMode, FileSide, SideIdentity,
};
use lectern_services::project::BlobId;
use lectern_services::review::{
    Brief, DiffSide, FileKey, LineAnchor, NoiseKind, ReviewTarget, SeenState,
};
use lectern_services::session::{SessionKey, SourceId, Timestamp};

use super::driven::{Driven, ReflogLine, write_reflog};
use super::fake::{FakeProjects, FakeSessions};
use crate::cutoff;

/// The source of the fixtures' sessions.
pub const SOURCE: &str = "session-claude-code";
const DAY_MS: i64 = 24 * 60 * 60 * 1000;

/// What [`SessionFixture::cutoffs`] computed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct CutoffRun {
    pub records: usize,
    /// Records whose changes are still unknown (0: all computed).
    pub unknown: usize,
}

/// A repository with `commits` commits, a HEAD reflog of `records`
/// records over the 14 days before the session (ten of them branch
/// switches), `files` changed files of 300 lines, and `sessions` sessions
/// in the worktree: the target ("s00", 20 turns, 120 edit calls over the
/// changed files), four more that changed some of them before it, the rest
/// on other files.
pub struct SessionFixture {
    pub repo: TestRepo,
    pub projects: Arc<FakeProjects>,
    pub sessions: Arc<FakeSessions>,
    pub now: Timestamp,
    src: Vec<String>,
}

fn line(i: usize, l: usize) -> String {
    format!("    let value_{i}_{l} = compute({l}, \"fixture line\");\n")
}

fn changed(i: usize, l: usize, tag: usize) -> String {
    format!("    let value_{i}_{l} = changed({l}, {tag});\n")
}

const LINES: usize = 300;

impl SessionFixture {
    /// The fixture of the design: `new(2000, 300, 50, 50)`.
    pub fn new(commits: usize, records: usize, sessions: usize, files: usize) -> SessionFixture {
        let repo = TestRepo::new();
        let root = repo.root().to_path_buf();
        let start_s: i64 = 1_760_000_000;
        let now = Timestamp((start_s + 4 * 3600) * 1000);
        let session_start = (start_s + 3600) * 1000;

        // The changed files and the other files of the history.
        let src: Vec<String> = (0..files)
            .map(|i| format!("src/m{:02}/f{i}.rs", i % 10))
            .collect();
        let base_texts: Vec<Vec<String>> = (0..files)
            .map(|i| (0..LINES).map(|l| line(i, l)).collect())
            .collect();
        let src_oids: Vec<Oid> = base_texts
            .iter()
            .map(|t| repo.blob(t.concat().as_bytes()))
            .collect();
        let hist: Vec<String> = (0..100)
            .map(|k| format!("hist/d{:02}/h{k}.txt", k % 20))
            .collect();
        let mut hist_versions: Vec<usize> = vec![0; hist.len()];
        let hist_blob =
            |k: usize, v: usize| repo.blob(format!("history {k} version {v}\n").as_bytes());
        let mut hist_oids: Vec<Oid> = (0..hist.len()).map(|k| hist_blob(k, 0)).collect();
        let tree = |hist_oids: &[Oid]| {
            let mut entries: Vec<(&str, Mode, Oid)> = src
                .iter()
                .zip(&src_oids)
                .map(|(p, o)| (p.as_str(), Mode::File, *o))
                .collect();
            entries.extend(
                hist.iter()
                    .zip(hist_oids)
                    .map(|(p, o)| (p.as_str(), Mode::File, *o)),
            );
            repo.tree_entries(&entries)
        };

        // The old history: one tree, many commits.
        let first = tree(&hist_oids);
        let old_commits = commits.saturating_sub(records).max(1);
        let mut head = repo.commit_at(first, &[], "initial", start_s - 400 * 86_400);
        for n in 1..old_commits {
            head = repo.commit_at(
                first,
                &[head],
                "old",
                start_s - 400 * 86_400 + n as i64 * 60,
            );
        }

        // The reflog's records over 13 days before the session.
        let mut lines: Vec<(Option<Oid>, Oid, i64, String)> = Vec::new();
        let span = 13 * 86_400;
        let t0 = start_s - span;
        let mut saved = head;
        let mut side = None;
        for r in 0..records {
            let time = t0 + (r as i64 * (span - 7200)) / records.max(1) as i64;
            if r % 60 == 10 && side.is_none() {
                let k = r % hist.len();
                let mut oids = hist_oids.clone();
                oids[k] = hist_blob(k, 10_000 + r);
                let s = repo.commit_at(tree(&oids), &[head], "side", time - 30);
                lines.push((
                    Some(head),
                    s,
                    time,
                    "checkout: moving from main to side".into(),
                ));
                saved = head;
                side = Some(s);
                head = s;
                continue;
            }
            if let Some(s) = side.take() {
                lines.push((
                    Some(s),
                    saved,
                    time,
                    "checkout: moving from side to main".into(),
                ));
                head = saved;
                continue;
            }
            let k = r % hist.len();
            hist_versions[k] += 1;
            hist_oids[k] = hist_blob(k, hist_versions[k]);
            let next = repo.commit_at(tree(&hist_oids), &[head], "commit", time - 10);
            lines.push((Some(head), next, time, format!("commit: change {k}")));
            head = next;
        }
        repo.set_ref("refs/heads/main", head);
        repo.set_head(TestHead::Branch("refs/heads/main"));
        let reflog: Vec<ReflogLine<'_>> = lines
            .iter()
            .map(|(old, new, time, message)| ReflogLine {
                old: *old,
                new: *new,
                time: *time,
                message,
            })
            .collect();
        write_reflog(repo.common_dir(), repo.hash(), &reflog);

        // Sessions and the worktree they leave.
        let mut texts = base_texts.clone();
        let mut script_sessions: Vec<Value> = Vec::new();
        let replace =
            |path: &str, old: &str, new: &str| json!({ "path": path, "replace": [old, new] });
        // Four sessions before the target, ten files each.
        let others = 4.min(sessions.saturating_sub(1));
        for k in 0..others {
            let at = session_start - (others - k) as i64 * 600_000;
            let mut items = Vec::new();
            for (n, i) in (k * 10..(k * 10 + 10).min(files)).enumerate() {
                let l = 280 + k;
                let new = changed(i, l, 900 + k);
                items.push(json!({
                    "edit": format!("o{k}-{n}"), "at": at + n as i64 * 1000,
                    "done": at + n as i64 * 1000 + 500,
                    "changes": [replace(&src[i], &line(i, l), &new)],
                }));
                texts[i][l] = new;
            }
            script_sessions.push(json!({
                "id": format!("s{:02}", k + 1), "cwd": root.to_string_lossy(), "started_at": at,
                "turns": [{ "id": format!("o{k}-t1"), "at": at, "prompt": "change things", "items": items }],
            }));
        }
        // The target: 20 turns, 6 edit calls each.
        let mut turns = Vec::new();
        for t in 0..20 {
            let at = session_start + t as i64 * 60_000;
            let mut items = vec![json!({ "text": format!("Turn {t}: editing.") })];
            for e in 0..6 {
                let c = t * 6 + e;
                let i = c % files;
                let l = 10 + (c / files) * 90 + (i % 7);
                let new = changed(i, l, c);
                items.push(json!({
                    "edit": format!("c{c}"), "at": at + e as i64 * 2000,
                    "done": at + e as i64 * 2000 + 800,
                    "changes": [replace(&src[i], &texts[i][l], &new)],
                }));
                texts[i][l] = new;
            }
            items.push(json!({ "run": format!("r{t}"), "command": "cargo check",
                "at": at + 20_000, "done": at + 25_000 }));
            turns.push(json!({ "id": format!("t{t}"), "at": at, "prompt": format!("Step {t}"), "items": items }));
        }
        script_sessions.insert(0, json!({
            "id": "s00", "cwd": root.to_string_lossy(), "started_at": session_start, "turns": turns,
        }));
        // The rest: older sessions on files of the history.
        for k in (others + 1)..sessions {
            let at = now.0 - (k as i64 % 13 + 1) * DAY_MS;
            let h = &hist[k % hist.len()];
            script_sessions.push(json!({
                "id": format!("s{k:02}"), "cwd": root.to_string_lossy(), "started_at": at,
                "turns": [{ "id": format!("x{k}"), "at": at, "prompt": "other work", "items": [
                    { "edit": format!("x{k}-e"), "at": at + 1000, "done": at + 1500,
                      "changes": [{ "path": h, "write": "rewritten\n" }] },
                    { "run": format!("x{k}-r"), "command": "make", "at": at + 2000, "done": at + 3000 },
                ]}],
            }));
        }
        let script = json!({ "agent": "claude-code", "adapter": 1, "sessions": script_sessions });
        let sessions = Arc::new(FakeSessions::from_script(SOURCE, &script.to_string()));

        // The worktree: the history files as committed, the changed files
        // as the sessions left them; the index has the committed blobs.
        let mut index = Vec::new();
        for (k, path) in hist.iter().enumerate() {
            let v = hist_versions[k];
            repo.write_file(path, format!("history {k} version {v}\n").as_bytes());
            repo.backdate(path);
        }
        for path in &hist {
            index.push(repo.add(path));
        }
        for (i, path) in src.iter().enumerate() {
            repo.write_file(path, texts[i].concat().as_bytes());
            index.push(IndexEntry::new(path, Mode::File, src_oids[i]));
        }
        repo.write_index(repo.common_dir(), &index);

        // Ten sub-projects hold the changed files (the overview's
        // per-package counts of today's changes).
        let dirs: Vec<String> = (0..10).map(|m| format!("src/m{m:02}")).collect();
        let dirs: Vec<&str> = dirs.iter().map(String::as_str).collect();
        let projects =
            Arc::new(FakeProjects::new(repo.common_dir(), repo.root()).with_packages(&dirs));
        SessionFixture {
            repo,
            projects,
            sessions,
            now,
            src,
        }
    }

    /// The target session's review.
    pub fn target(&self) -> ReviewTarget {
        ReviewTarget::Session(SessionKey {
            source: SourceId(SOURCE.into()),
            id: "s00".into(),
        })
    }

    /// The file [`SessionFixture::agent_step`] changes.
    pub fn agent_path(&self) -> String {
        self.src[0].clone()
    }

    /// The agent's next step on the target session (`review/refresh_one`):
    /// a turn whose edit rewrites the first changed file with one more
    /// line, and the same content written to the worktree. Returns the
    /// file's path and the `SessionsChanged` the source recorded. `n`
    /// counts the steps, from 0.
    pub fn agent_step(&self, n: usize) -> (String, lectern_services::session::SessionsChanged) {
        let path = self.agent_path();
        let file = self.repo.root().join(&path);
        let mut text = String::from_utf8(std::fs::read(&file).unwrap()).unwrap();
        text.push_str(&format!("// agent step {n}\n"));
        let at = (self.now.0 - 3 * 3600 * 1000) + (21 * 60_000) + n as i64 * 10_000;
        let turn = json!({ "id": format!("n{n}"), "at": at, "prompt": "one more", "items": [
            { "edit": format!("n{n}-e"), "at": at + 1000, "done": at + 1500,
              "changes": [{ "path": path, "write": text }] },
        ]});
        self.repo.write_file(&path, text.as_bytes());
        self.sessions.append("s00", &turn.to_string());
        let event = self.sessions.take_events().pop().expect("an event");
        (path, event)
    }

    /// An engine on this fixture with its cache and logs under `dir`.
    pub fn driven(&self, dir: &Path) -> Driven {
        Driven::new(
            Arc::clone(&self.projects),
            vec![Arc::clone(&self.sessions)],
            dir,
            self.now,
        )
    }

    /// Builds the cutoff table of the changed files for every record of
    /// the window, with a new cache under `cache_dir`: every
    /// `tree_changes` a miss. Blocking.
    pub fn cutoffs(&self, cache_dir: &Path) -> CutoffRun {
        let cache = Cache::new(CacheConfig {
            root: cache_dir.to_path_buf(),
            budgets: Budgets::default(),
            ui_thread: None,
        });
        let repo = self.repo.repo();
        let worktree = repo.worktree(&lectern_git::WorktreeId::Main).unwrap();
        let window_s = (self.now.0 - cutoff::WINDOW_MS) / 1000;
        let reflog = worktree.head_reflog(window_s, 4096).unwrap();
        // The target's base is the newest record: nothing comes after it.
        let (records, _) = cutoff::window(&reflog.entries, 0, window_s, &mut |_| false);
        let paths: HashSet<ChangePath> = self
            .src
            .iter()
            .map(|p| ChangePath(Arc::from(p.as_bytes())))
            .collect();
        let table = cutoff::build(
            &repo,
            &cache.repo(&repo.location().common_dir),
            &records,
            paths,
            None,
            &AtomicBool::new(false),
        );
        let unknown = table.unknown(&records).len();
        cache.close();
        CutoffRun {
            records: records.len(),
            unknown,
        }
    }
}

/// A worktree with `files` untracked files and no commit, for the seen
/// command on a 5000-file review (`review/mark_seen_5000`).
pub struct MarkSeenFixture {
    pub repo: TestRepo,
    pub projects: Arc<FakeProjects>,
    pub files: usize,
}

impl MarkSeenFixture {
    pub fn new(files: usize) -> MarkSeenFixture {
        let repo = TestRepo::new();
        for i in 0..files {
            repo.write_file(
                &format!("gen/d{:03}/f{i}.txt", i % 100),
                format!("{i}\n").as_bytes(),
            );
        }
        let projects = Arc::new(FakeProjects::new(repo.common_dir(), repo.root()));
        MarkSeenFixture {
            repo,
            projects,
            files,
        }
    }

    /// Writes the annotation log of the main worktree's changes outside any
    /// session (what [`Driven::outside`] reviews) with `pending` notes not
    /// sent and `sent` sent in batches of 40, on the fixture's files, for an
    /// engine on `dir` (`review/note_add`).
    pub fn seed_notes(&self, dir: &Path, pending: usize, sent: usize) -> std::io::Result<()> {
        let target = crate::seen::TargetKey::Outside {
            worktree: Arc::from(""),
        };
        let files = self.files.max(1);
        let path = |i: usize| {
            let n = i % files;
            ChangePath(Arc::from(
                format!("gen/d{:03}/f{n}.txt", n % 100).as_bytes(),
            ))
        };
        let mut file = crate::log::LogFile::new(&dir.join("review"), self.repo.common_dir());
        let (_, report) = file.load(Timestamp(0));
        if let Some(error) = report.error {
            return Err(error);
        }
        file.append(&super::note_records(&target, pending, sent, path))
    }

    pub fn driven(&self, dir: &Path) -> Driven {
        let sessions = Arc::new(FakeSessions::from_script(SOURCE, r#"{"sessions": []}"#));
        Driven::new(
            Arc::clone(&self.projects),
            vec![sessions],
            dir,
            Timestamp(1_760_000_000_000),
        )
    }
}

/// The inputs of a rule brief over `files` files, every rule represented
/// (`review/brief_5000`): no repository, no engine, the rules alone.
pub struct BriefFixture {
    files: Vec<BriefInput>,
}

struct BriefInput {
    key: FileKey,
    change: FileChange,
    noise: Option<NoiseKind>,
    seen: SeenState,
    drifted: bool,
    edit_failed: bool,
    command: Vec<(u32, u32)>,
}

impl BriefFixture {
    pub fn new(files: usize) -> BriefFixture {
        let blob = |n: usize| FileSide {
            mode: FileMode::File,
            content: SideIdentity::Blob(
                BlobId::from_bytes(&(n as u64).to_le_bytes().repeat(3)[..20]).unwrap(),
            ),
        };
        let files = (0..files)
            .map(|i| {
                let path = match i % 19 {
                    0 if i % 13 == 0 => format!("gen/x{i}.lock"),
                    _ if i % 13 == 0 => format!(".github/workflows/w{i}.yml"),
                    _ => format!("src/m{:02}/f{i}.rs", i % 50),
                };
                let deleted = i % 17 == 0;
                let detail = if deleted {
                    ChangeDetail::NoDiff
                } else if i % 23 == 0 {
                    ChangeDetail::Truncated {
                        added: 30_000,
                        removed: 1,
                    }
                } else {
                    ChangeDetail::Text {
                        hunks: Arc::from(vec![DiffHunk {
                            old: 0..1,
                            new: 0..2,
                            old_byte: 0,
                            new_byte: 0,
                        }]),
                        added: 1,
                        removed: 0,
                    }
                };
                let change = FileChange {
                    status: if deleted {
                        ChangeStatus::Deleted
                    } else {
                        ChangeStatus::Modified
                    },
                    path: ChangePath(Arc::from(path.as_bytes())),
                    old: Some(blob(i)),
                    new: (!deleted).then(|| blob(i + 1)),
                    detail,
                    generated: false,
                    untracked: false,
                    staged: None,
                };
                BriefInput {
                    key: FileKey {
                        old: Some(change.path.clone()),
                        new: change.new.as_ref().map(|_| change.path.clone()),
                    },
                    noise: path.ends_with(".lock").then_some(NoiseKind::Lockfile),
                    seen: if i % 7 == 0 {
                        SeenState::ChangedSinceSeen {
                            since: Timestamp(1),
                        }
                    } else {
                        SeenState::Unseen
                    },
                    drifted: i % 11 == 0,
                    edit_failed: i % 29 == 0,
                    command: if i % 31 == 0 {
                        vec![(0, (i % 5) as u32)]
                    } else {
                        Vec::new()
                    },
                    change,
                }
            })
            .collect();
        BriefFixture { files }
    }

    /// The brief of the files (「简报规则」), no providers.
    pub fn brief(&self) -> Brief {
        let inputs: Vec<crate::brief::FileInput<'_>> = self
            .files
            .iter()
            .map(|f| crate::brief::FileInput {
                key: &f.key,
                change: &f.change,
                noise: f.noise,
                seen: f.seen,
                drifted: f.drifted,
                drifted_at: f.drifted.then_some(LineAnchor {
                    side: DiffSide::New,
                    line: 0,
                }),
                edit_failed: f.edit_failed,
                command_only: &f.command,
            })
            .collect();
        crate::brief::build(
            &inputs,
            &[],
            0,
            lectern_services::review::AnalysisState::Unavailable(Arc::from("unavailable")),
        )
    }
}
