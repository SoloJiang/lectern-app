//! The fixture of `app/open_diff` (docs/design/review_view.md「性能」) and of
//! the manual check of the review screen: a worktree in which `modules`
//! files of about 300 lines were changed (10 lines each), and a Claude Code
//! session log that made those changes with `Write` calls, written where the
//! measurement build looks for one (`<HOME>/.claude/projects/`). The app
//! discovers, places and attributes it like a real session; nothing here
//! calls `agents` or `review`. The files sit in `PACKAGES` sub-projects with a
//! `package.json` each, so the project home has sub-projects with changes
//! today. Built with `TestRepo` and the modules of `git_worktree`.

use std::io;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use lectern_git::testing::TestRepo;
use serde_json::{Value, json};

use crate::git_worktree::module;

/// The session's `sessionId`.
pub const SESSION_ID: &str = "0a11ce00-1111-4111-8111-000000000001";
/// The plugin ID of the Claude Code session source.
const SOURCE: &str = "session-claude-code";
/// Sub-projects, each with a `package.json`.
const PACKAGES: usize = 5;

fn module_path(n: usize) -> String {
    format!("packages/p{:02}/src/module{n:02}.rs", n % PACKAGES)
}

pub struct ReviewSession {
    pub repo: TestRepo,
    log: Vec<u8>,
}

impl ReviewSession {
    /// `files` tracked files (`PACKAGES` of them manifests), of which
    /// `modules` were rewritten by the
    /// session in `turns` turns (the files spread over the turns in order).
    /// The session ran in the last half hour before `now`.
    pub fn new(files: usize, modules: usize, turns: usize, now: SystemTime) -> ReviewSession {
        let repo = TestRepo::new();
        let mut contents: Vec<(String, Vec<u8>)> = (0..PACKAGES)
            .map(|k| {
                (
                    format!("packages/p{k:02}/package.json"),
                    format!("{{ \"name\": \"p{k:02}\" }}\n").into_bytes(),
                )
            })
            .collect();
        contents.extend((0..files.saturating_sub(modules + PACKAGES)).map(|i| {
            (
                format!("packages/p{:02}/src/file{i:03}.ts", i % PACKAGES),
                format!("export const x{i} = 0;\n").into_bytes(),
            )
        }));
        contents.extend((0..modules).map(|n| (module_path(n), module(n, 0, false))));
        for (path, content) in &contents {
            repo.write_file(path, content);
            repo.backdate(path);
        }
        let refs: Vec<(&str, &[u8])> = contents
            .iter()
            .map(|(p, c)| (p.as_str(), c.as_slice()))
            .collect();
        let commit = repo.commit(repo.tree(&refs), &[], "base");
        repo.set_ref("refs/heads/main", commit);
        let entries: Vec<_> = contents.iter().map(|(p, _)| repo.add(p)).collect();
        repo.write_index(repo.common_dir(), &entries);
        // What the session left: every module rewritten, modified now.
        for n in 0..modules {
            repo.write_file(&module_path(n), &module(n, 0, true));
        }
        let root = repo.root().to_string_lossy().into_owned();
        let start = now - Duration::from_secs(30 * 60);
        let mut log = Log::new(&root, start);
        let per_turn = modules.div_ceil(turns.max(1)).max(1);
        for (t, chunk) in (0..modules)
            .collect::<Vec<_>>()
            .chunks(per_turn)
            .enumerate()
        {
            log.prompt(&format!("Step {t}: change the next modules."));
            log.text(&format!("Turn {t}: rewriting {} modules.", chunk.len()));
            for &n in chunk {
                let path = format!("{root}/{}", module_path(n));
                let old = String::from_utf8(module(n, 0, false)).expect("UTF-8");
                let new = String::from_utf8(module(n, 0, true)).expect("UTF-8");
                log.write(&path, &old, &new);
            }
            log.text("Done.");
        }
        ReviewSession {
            repo,
            log: log.bytes,
        }
    }

    pub fn root(&self) -> &Path {
        self.repo.root()
    }

    /// The session's key as `Driver::open(Diff)` takes it.
    pub fn session(&self) -> String {
        format!("{SOURCE}:{SESSION_ID}")
    }

    /// Writes the session log under `home` as Claude Code does:
    /// `.claude/projects/<the root with '/' as '-'>/<sessionId>.jsonl`.
    pub fn write_home(&self, home: &Path) -> io::Result<()> {
        let encoded = self.root().to_string_lossy().replace('/', "-");
        let dir = home.join(".claude/projects").join(encoded);
        std::fs::create_dir_all(&dir)?;
        std::fs::write(dir.join(format!("{SESSION_ID}.jsonl")), &self.log)
    }
}

/// The records of one session, each a second after the one before.
struct Log<'a> {
    cwd: &'a str,
    at: SystemTime,
    n: usize,
    parent: Option<String>,
    bytes: Vec<u8>,
}

impl<'a> Log<'a> {
    fn new(cwd: &'a str, at: SystemTime) -> Log<'a> {
        Log {
            cwd,
            at,
            n: 0,
            parent: None,
            bytes: Vec::new(),
        }
    }

    fn record(&mut self, kind: &str, message: Value, result: Option<Value>) {
        self.n += 1;
        let uuid = format!("0a11ce00-0000-4000-8000-{:012}", self.n);
        let mut record = json!({
            "parentUuid": self.parent,
            "isSidechain": false,
            "userType": "external",
            "cwd": self.cwd,
            "sessionId": SESSION_ID,
            "version": "2.1.302",
            "gitBranch": "main",
            "type": kind,
            "message": message,
            "uuid": uuid,
            "timestamp": timestamp(self.at),
        });
        if let Some(result) = result {
            record["toolUseResult"] = result;
        }
        self.bytes.extend_from_slice(record.to_string().as_bytes());
        self.bytes.push(b'\n');
        self.parent = Some(uuid);
        self.at += Duration::from_secs(1);
    }

    fn prompt(&mut self, text: &str) {
        self.record("user", json!({ "role": "user", "content": text }), None);
    }

    fn text(&mut self, text: &str) {
        let id = format!("msg_fixture_{:06}", self.n);
        self.record(
            "assistant",
            json!({
                "id": id, "type": "message", "role": "assistant", "model": "fixture-model",
                "content": [{ "type": "text", "text": text }],
                "stop_reason": null, "stop_sequence": null,
                "usage": { "input_tokens": 10, "output_tokens": 20 },
            }),
            None,
        );
    }

    /// A successful `Write` of `new` over `old` at the absolute `path`.
    fn write(&mut self, path: &str, old: &str, new: &str) {
        let call = format!("toolu_fixture_{:06}", self.n);
        let id = format!("msg_fixture_{:06}", self.n);
        self.record(
            "assistant",
            json!({
                "id": id, "type": "message", "role": "assistant", "model": "fixture-model",
                "content": [{ "type": "tool_use", "id": call, "name": "Write",
                              "input": { "file_path": path, "content": new } }],
                "stop_reason": "tool_use", "stop_sequence": null,
                "usage": { "input_tokens": 3, "output_tokens": 60 },
            }),
            None,
        );
        self.record(
            "user",
            json!({ "role": "user", "content": [{
                "tool_use_id": call, "type": "tool_result",
                "content": format!("The file {path} has been updated."),
            }]}),
            Some(json!({
                "type": "update", "filePath": path, "content": new,
                "originalFile": old, "structuredPatch": [],
            })),
        );
    }
}

/// RFC 3339 in UTC with milliseconds, as Claude Code writes it.
fn timestamp(at: SystemTime) -> String {
    let since = at.duration_since(UNIX_EPOCH).unwrap_or_default();
    let secs = since.as_secs() as i64;
    let (days, rest) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let (y, m, d) = civil(days);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{:03}Z",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60,
        since.subsec_millis()
    )
}

/// The proleptic Gregorian date of a day count since 1970-01-01.
fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}
