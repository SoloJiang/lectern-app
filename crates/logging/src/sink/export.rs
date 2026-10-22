//! Export: the recent runs' files and a manifest copied into a new
//! directory, for the user to attach to an issue. Runs on the caller's
//! thread (an executor `Background` task).

use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::files::{self, Listed};
use super::{Logging, fs};
use crate::clock;
use crate::engine::time;

#[derive(Clone, Debug)]
pub struct ExportOptions {
    /// The most recent runs to include; 3 by default.
    pub runs: usize,
    /// Whether to include those runs' verbose files; yes by default.
    pub include_verbose: bool,
    /// Further files to copy (crash reports), listed by `app` through
    /// `platform` (「未决问题」3).
    pub extra: Vec<PathBuf>,
}

impl Default for ExportOptions {
    fn default() -> ExportOptions {
        ExportOptions {
            runs: 3,
            include_verbose: true,
            extra: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Exported {
    pub dir: PathBuf,
    /// Files written, the manifest included.
    pub files: usize,
    pub bytes: u64,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ExportError {
    NoDirectory,
    Io(io::ErrorKind),
}

impl From<io::Error> for ExportError {
    fn from(e: io::Error) -> ExportError {
        ExportError::Io(e.kind())
    }
}

/// A run: its (stamp, pid) and its files.
struct Run {
    key: (String, u32),
    files: Vec<Listed>,
}

fn runs(files: Vec<Listed>) -> Vec<Run> {
    let mut runs: Vec<Run> = Vec::new();
    for f in files {
        let key = (f.name.stamp.clone(), f.name.pid);
        match runs.last_mut() {
            Some(run) if run.key == key => run.files.push(f),
            _ => runs.push(Run {
                key,
                files: vec![f],
            }),
        }
    }
    runs
}

pub(crate) fn export(
    logging: &Logging,
    parent: &Path,
    options: &ExportOptions,
) -> Result<Exported, ExportError> {
    let state = logging.state();
    let dir = state.dir.as_ref().ok_or(ExportError::NoDirectory)?;
    state.flush(Duration::from_secs(1), false);
    let mut chosen = runs(files::list(dir));
    let keep = chosen.len().saturating_sub(options.runs);
    chosen.drain(..keep);
    let out = new_dir(parent)?;
    let mut written = 0;
    let mut bytes = 0;
    let mut manifest_files = Vec::new();
    let mut manifest_runs = Vec::new();
    let verbose: Vec<Listed> = if options.include_verbose {
        files::list(&files::verbose_dir(dir))
    } else {
        Vec::new()
    };
    let has_verbose = verbose.iter().any(|v| {
        chosen
            .iter()
            .any(|r| r.key == (v.name.stamp.clone(), v.name.pid))
    });
    if has_verbose {
        fs::create_dir_all(&out.join("verbose"))?;
    }
    for run in &chosen {
        let run_id = run
            .files
            .first()
            .and_then(|f| fs::head_and_tail(&f.path, 4096).ok())
            .and_then(|(head, _)| files::run_id(&String::from_utf8_lossy(&head)));
        manifest_runs.push(run_id);
        let own_verbose = verbose
            .iter()
            .filter(|v| (v.name.stamp.clone(), v.name.pid) == run.key);
        for (f, sub) in run
            .files
            .iter()
            .map(|f| (f, ""))
            .chain(own_verbose.map(|f| (f, "verbose/")))
        {
            let name = format!(
                "{sub}{}",
                files::file_name(&f.name.stamp, f.name.pid, f.name.seq)
            );
            let n = fs::copy(&f.path, &out.join(&name))?;
            written += 1;
            bytes += n;
            manifest_files.push((name, n));
        }
    }
    if !options.extra.is_empty() {
        fs::create_dir_all(&out.join("extra"))?;
    }
    for (i, extra) in options.extra.iter().enumerate() {
        let file_name = extra.file_name().and_then(|n| n.to_str()).unwrap_or("file");
        let name = format!("extra/{i}-{file_name}");
        let n = fs::copy(extra, &out.join(&name))?;
        written += 1;
        bytes += n;
        manifest_files.push((name, n));
    }
    let manifest = manifest(logging, &manifest_runs, &manifest_files);
    fs::write_new(&out.join("manifest.json"), manifest.as_bytes())?;
    Ok(Exported {
        dir: out,
        files: written + 1,
        bytes: bytes + manifest.len() as u64,
    })
}

/// `lectern-logs-<stamp>` under `parent`, with `-<n>` when it exists.
fn new_dir(parent: &Path) -> Result<PathBuf, ExportError> {
    let base = format!("lectern-logs-{}", time::file_stamp(clock::now()));
    for n in 0..100 {
        let name = if n == 0 {
            base.clone()
        } else {
            format!("{base}-{n}")
        };
        let path = parent.join(name);
        if fs::exists(&path) {
            continue;
        }
        fs::create_dir(&path)?;
        return Ok(path);
    }
    Err(ExportError::Io(io::ErrorKind::AlreadyExists))
}

fn manifest(logging: &Logging, runs: &[Option<u64>], files: &[(String, u64)]) -> String {
    let status = logging.status();
    let runs: Vec<String> = runs
        .iter()
        .map(|r| r.map_or_else(|| "null".to_owned(), |r| format!("\"{r:016x}\"")))
        .collect();
    let files: Vec<String> = files
        .iter()
        .map(|(name, bytes)| format!("{{\"name\":\"{}\",\"bytes\":{bytes}}}", escape(name)))
        .collect();
    format!(
        "{{\"format\":1,\"exported\":\"{}\",\"runs\":[{}],\"files\":[{}],\"dropped\":{},\"suppressed\":{}}}\n",
        time::rfc3339_s(clock::now()),
        runs.join(","),
        files.join(","),
        status.dropped,
        status.suppressed,
    )
}

fn escape(s: &str) -> String {
    s.chars()
        .flat_map(|c| match c {
            '"' | '\\' => vec!['\\', c],
            c if (c as u32) < 0x20 => format!("\\u{:04x}", c as u32).chars().collect(),
            c => vec![c],
        })
        .collect()
}
