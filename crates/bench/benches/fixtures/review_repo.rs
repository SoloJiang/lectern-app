//! A repository shaped like a project fixture with sessions in its
//! worktree, for `review/overview_cloudprint` and `review/resident_*`
//! (docs/design/review.md「性能」). The cloudprint shape is written whole:
//! every path committed with a line of content and present in the
//! worktree, `edited` of them changed by one session each. The large shape
//! is index-only (as `project_repo`): the count comparison lists every
//! path as missing from the worktree, more than the 4000 the overview
//! keeps, so the overview is capped, which is what that shape measures.

use std::sync::Arc;

use lectern_bench::BenchError;
use lectern_git::testing::{IndexEntry, TestHead, TestRepo};
use lectern_git::{Mode, Oid};
use lectern_project::testing::Shape;
use lectern_review::testing::{FakeProjects, FakeSessions};
use lectern_services::session::Timestamp;

pub struct ReviewRepo {
    pub t: TestRepo,
    pub projects: Arc<FakeProjects>,
    pub sessions: Arc<FakeSessions>,
    pub now: Timestamp,
    /// The paths the sessions edited.
    pub edited: Vec<String>,
}

const SOURCE: &str = "session-claude-code";

/// Unix seconds the sessions happen around.
const T: i64 = 1_760_000_000;

/// `shape` with `sessions` sessions, each editing one of its source files.
/// `whole`: every path committed and in the worktree.
pub fn build(shape: &Shape, whole: bool, sessions: usize) -> Result<ReviewRepo, BenchError> {
    let t = TestRepo::new();
    let hash = t.hash();
    let content = |path: &str| format!("// {path}\nfn f() {{}}\n");
    let tree = if whole {
        let files: Vec<(&str, Vec<u8>)> = shape
            .paths
            .iter()
            .map(|p| (p.as_str(), content(p).into_bytes()))
            .collect();
        let files: Vec<(&str, &[u8])> = files.iter().map(|(p, c)| (*p, c.as_slice())).collect();
        for (path, bytes) in &files {
            t.write_file(path, bytes);
        }
        t.tree(&files)
    } else {
        t.tree(&[("README.md", b"# repo\n")])
    };
    let commit = t.commit_at(tree, &[], "base", T - 86_400);
    t.set_ref("refs/heads/main", commit);
    t.set_head(TestHead::Branch("refs/heads/main"));
    let entries: Vec<IndexEntry> = shape
        .paths
        .iter()
        .map(|p| {
            let oid = if whole {
                Oid::for_blob(hash, content(p).as_bytes())
            } else {
                Oid::for_blob(hash, p.as_bytes())
            };
            IndexEntry::new(p, Mode::File, oid)
        })
        .collect();
    t.write_index(t.common_dir(), &entries);
    if whole {
        for p in &shape.paths {
            t.backdate(p);
        }
    }
    // The sessions: each edits one source file (a line appended).
    let edited: Vec<String> = shape
        .paths
        .iter()
        .filter(|p| !shape.manifests.iter().any(|(m, _)| m == *p))
        .take(sessions)
        .cloned()
        .collect();
    if edited.len() < sessions {
        return Err(BenchError::new("not enough source files for the sessions"));
    }
    let root = t.root().to_string_lossy().into_owned();
    let mut script = Vec::new();
    for (i, path) in edited.iter().enumerate() {
        let old = content(path);
        let new = format!("{old}// edited by s{i}\n");
        if whole {
            t.write_file(path, new.as_bytes());
        }
        let at = (T + i as i64 * 60) * 1000;
        script.push(serde_json::json!({
            "id": format!("5e55e55e-3333-4333-8333-{i:012}"),
            "cwd": root,
            "started_at": at,
            "turns": [{
                "id": "t1", "at": at + 10, "prompt": "edit",
                "items": [{
                    "edit": "c1", "at": at + 20, "done": at + 500,
                    "changes": [{ "path": path, "replace": [old, new] }]
                }]
            }]
        }));
    }
    let sessions = Arc::new(FakeSessions::from_script(
        SOURCE,
        &serde_json::json!({ "sessions": script }).to_string(),
    ));
    let projects = Arc::new(FakeProjects::new(t.common_dir(), t.root()));
    Ok(ReviewRepo {
        t,
        projects,
        sessions,
        now: Timestamp((T + 3600) * 1000),
        edited,
    })
}
