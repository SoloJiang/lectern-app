//! Debug builds only: `--quit-after-start` for `tests/startup.rs`. Once the
//! barrier is released and the writer has created this run's log file, quit
//! through the `Quit` action or, with `=system`, through `cx.quit()`
//! directly (the path a system quit takes).

use std::path::Path;
use std::time::Duration;

use gpui::App;

use crate::args::QuitPath;
use crate::startup::Started;

const POLL: Duration = Duration::from_millis(10);

pub fn init(cx: &mut App, path: Option<QuitPath>) {
    let Some(path) = path else {
        return;
    };
    cx.spawn(async move |cx| {
        loop {
            let dir = cx.update(|cx| {
                cx.try_global::<Started>()
                    .map(|started| started.logging.status().dir)
            });
            if let Some(dir) = dir {
                let has_file = match dir {
                    Some(dir) => {
                        cx.background_executor()
                            .spawn(async move { has_log_file(&dir) })
                            .await
                    }
                    None => true,
                };
                if has_file {
                    break;
                }
            }
            cx.background_executor().timer(POLL).await;
        }
        cx.update(|cx| match path {
            QuitPath::Action => crate::quit::quit(cx),
            QuitPath::System => cx.quit(),
        });
    })
    .detach();
}

fn has_log_file(dir: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    entries.flatten().any(|entry| {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        name.starts_with("lectern-") && name.ends_with(".log")
    })
}
