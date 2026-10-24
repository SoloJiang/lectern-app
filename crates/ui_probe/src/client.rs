//! The blocking client of the probe protocol, for `lectern-probe` and for
//! the `app` integration test (docs/design/ui_probe.md「客户端」). Standard
//! library and `serde_json` only.

use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use serde::Deserialize;
use serde_json::json;

use crate::protocol::PROTOCOL;
use crate::server::SOCKET;

/// A response as the client reads it.
#[derive(Debug, Deserialize)]
pub struct Reply {
    pub v: u32,
    pub id: u64,
    pub ok: bool,
    #[serde(default)]
    pub result: Option<serde_json::Value>,
    #[serde(default)]
    pub error: Option<ReplyError>,
}

#[derive(Debug, Deserialize)]
pub struct ReplyError {
    /// The kebab-case `ErrorKind`.
    pub kind: String,
    pub message: String,
}

pub struct Client {
    writer: UnixStream,
    reader: BufReader<UnixStream>,
    next: u64,
}

impl Client {
    /// Connects to `dir/probe.sock`; `timeout` is how long each response is
    /// waited for.
    pub fn connect(dir: &Path, timeout: Duration) -> io::Result<Client> {
        let writer = UnixStream::connect(dir.join(SOCKET))?;
        writer.set_read_timeout(Some(timeout))?;
        writer.set_write_timeout(Some(timeout))?;
        let reader = BufReader::new(writer.try_clone()?);
        Ok(Client {
            writer,
            reader,
            next: 1,
        })
    }

    /// Sends one request and waits for its response. `Err` is a failure of
    /// the connection; an error the probe reports is in `Reply::error`.
    pub fn call(&mut self, method: &str, params: serde_json::Value) -> io::Result<Reply> {
        let id = self.next;
        self.next += 1;
        let request = json!({ "v": PROTOCOL, "id": id, "method": method, "params": params });
        let line = self.raw(&request.to_string())?;
        serde_json::from_str(&line).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
    }

    /// Sends `request` as one line, as it is, and returns the response line
    /// without its newline.
    pub fn raw(&mut self, request: &str) -> io::Result<String> {
        self.writer.write_all(request.as_bytes())?;
        self.writer.write_all(b"\n")?;
        let mut line = String::new();
        let read = self.reader.read_line(&mut line)?;
        if read == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        while line.ends_with(['\n', '\r']) {
            line.pop();
        }
        Ok(line)
    }
}
