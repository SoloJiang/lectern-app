//! `lectern-probe`: the command line client of the UI probe
//! (docs/design/ui_probe.md「客户端」). The result of a call goes to stdout,
//! a protocol error to stderr with exit code 1, a connection that cannot be
//! made to stderr with exit code 2, a usage error exit code 64.

#![allow(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "a command line tool prints its result and its errors (docs/design/logging.md「隐私」)"
)]

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use lectern_ui_probe::client::Client;
use serde_json::json;

const USAGE: &str = "\
Usage: lectern-probe [--dir <probe dir>] [--timeout <seconds>] <command>

Commands:
  hello                                      protocol, process and windows
  ui_tree                                    the semantic tree (ui-tree/1)
  screenshot [--name <name>] [--out <dir>] [--no-annotate]
                                             the window as a PNG; prints its path
  quit                                       quit the application like Cmd-Q
  raw '<one line of JSON>'                   send a request as it is

--dir defaults to $LECTERN_UI_PROBE_DIR. --timeout is how long each response
is waited for (default 30). The result is printed to stdout.
";

const DEFAULT_TIMEOUT: u64 = 30;

enum Command {
    Method(&'static str, serde_json::Value),
    Raw(String),
}

struct Args {
    dir: Option<PathBuf>,
    timeout: Duration,
    command: Command,
}

enum Parsed {
    Help,
    Run(Args),
}

fn parse(args: impl Iterator<Item = String>) -> Result<Parsed, String> {
    let mut args = args.peekable();
    let mut dir = None;
    let mut timeout = Duration::from_secs(DEFAULT_TIMEOUT);
    loop {
        match args.peek().map(String::as_str) {
            Some("--help" | "-h") => return Ok(Parsed::Help),
            Some("--dir") => {
                args.next();
                dir = Some(PathBuf::from(args.next().ok_or("--dir needs a value")?));
            }
            Some("--timeout") => {
                args.next();
                let seconds = args.next().ok_or("--timeout needs a value")?;
                let seconds: f64 = seconds
                    .parse()
                    .map_err(|_| "--timeout is a number of seconds")?;
                if !(seconds.is_finite() && seconds > 0.0) {
                    return Err("--timeout is a positive number of seconds".into());
                }
                timeout = Duration::from_secs_f64(seconds);
            }
            _ => break,
        }
    }
    let command = args.next().ok_or("a command is needed")?;
    let command = match command.as_str() {
        "hello" => Command::Method("hello", json!({})),
        "ui_tree" => Command::Method("ui_tree", json!({})),
        "quit" => Command::Method("quit", json!({})),
        "raw" => Command::Raw(args.next().ok_or("raw needs a request line")?),
        "screenshot" => {
            let mut params = serde_json::Map::new();
            while let Some(option) = args.next() {
                match option.as_str() {
                    "--name" => {
                        let name = args.next().ok_or("--name needs a value")?;
                        params.insert("name".into(), name.into());
                    }
                    "--out" => {
                        let out = args.next().ok_or("--out needs a value")?;
                        // The probe cannot resolve a path against this
                        // process's working directory.
                        let out =
                            std::path::absolute(&out).map_err(|_| "--out is not a usable path")?;
                        params.insert("dir".into(), out.to_string_lossy().into_owned().into());
                    }
                    "--no-annotate" => {
                        params.insert("annotate".into(), false.into());
                    }
                    other => return Err(format!("unknown option {other}")),
                }
            }
            Command::Method("screenshot", params.into())
        }
        other => return Err(format!("unknown command {other}")),
    };
    if let Some(extra) = args.next() {
        return Err(format!("unexpected argument {extra}"));
    }
    Ok(Parsed::Run(Args {
        dir,
        timeout,
        command,
    }))
}

fn main() -> ExitCode {
    let args = match parse(std::env::args().skip(1)) {
        Ok(Parsed::Help) => {
            print!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Ok(Parsed::Run(args)) => args,
        Err(message) => {
            eprintln!("lectern-probe: {message}\n\n{USAGE}");
            return ExitCode::from(64);
        }
    };
    let Some(dir) = args
        .dir
        .or_else(|| std::env::var_os("LECTERN_UI_PROBE_DIR").map(PathBuf::from))
    else {
        eprintln!("lectern-probe: no probe directory: pass --dir or set LECTERN_UI_PROBE_DIR");
        return ExitCode::from(64);
    };
    let mut client = match Client::connect(&dir, args.timeout) {
        Ok(client) => client,
        Err(e) => {
            eprintln!("lectern-probe: cannot connect: {e}");
            return ExitCode::from(2);
        }
    };
    match args.command {
        Command::Raw(line) => match client.raw(&line) {
            Ok(response) => {
                println!("{response}");
                let ok = serde_json::from_str::<serde_json::Value>(&response)
                    .ok()
                    .and_then(|v| v.get("ok").and_then(serde_json::Value::as_bool))
                    .unwrap_or(false);
                if ok {
                    ExitCode::SUCCESS
                } else {
                    ExitCode::from(1)
                }
            }
            Err(e) => {
                eprintln!("lectern-probe: connection failed: {e}");
                ExitCode::from(2)
            }
        },
        Command::Method(method, params) => match client.call(method, params) {
            Ok(reply) if reply.ok => {
                let result = reply.result.unwrap_or(serde_json::Value::Null);
                println!("{result}");
                ExitCode::SUCCESS
            }
            Ok(reply) => {
                match reply.error {
                    Some(error) => eprintln!("lectern-probe: {}: {}", error.kind, error.message),
                    None => eprintln!("lectern-probe: the probe reported an error"),
                }
                ExitCode::from(1)
            }
            Err(e) => {
                eprintln!("lectern-probe: connection failed: {e}");
                ExitCode::from(2)
            }
        },
    }
}
