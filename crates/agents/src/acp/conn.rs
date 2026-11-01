//! 一个连接的进程与管道（docs/design/agents.md「ACP 客户端」的「启动」「运行、关闭、权限」）。
//!
//! 进程在自己的进程组里启动，三条管道都接到 Lectern。读 stdout 的循环在专用线程上运行
//! （[`read_loop`]，不启动异步运行时），把一行一行的消息交给 [`Sink`]；写 stdin 是一个队列，
//! 谁先放进去谁先写，由 executor 任务或关闭的调用者排空（[`Conn::drain`]、[`Conn::close`]），读线程
//! 自己从不写，所以两条管道同时写满也不会互相等死。stderr 由另一个线程读掉，只数字节。
//! 本模块不认识 JSON-RPC，也不取引擎的锁。

use std::collections::VecDeque;
use std::ffi::OsString;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use rustix::process::{Pid, Signal, kill_process_group};

use super::wire::MAX_LINE;

/// 怎样启动：程序、工作目录和环境（登录 shell 的环境去掉 `LECTERN_` 开头的变量）。没有参数。
pub(crate) struct Spec {
    pub(crate) program: PathBuf,
    pub(crate) cwd: PathBuf,
    pub(crate) env: Vec<(OsString, OsString)>,
}

/// 读线程把进程的动静交给谁（引擎）。回调在读线程上运行，不得阻塞很久。
pub(crate) trait Sink: Send + Sync {
    /// 一行消息（不含换行，不为空）。
    fn line(&self, conn: &Arc<Conn>, line: &[u8]);
    /// stdout 不会再有消息：对端关闭，或读不下去（行太长、读错误）。之后读线程等进程退出。
    fn ended(&self, conn: &Arc<Conn>, end: End);
    /// 进程已被回收。
    fn exited(&self, conn: &Arc<Conn>, exit: Exit);
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum End {
    Eof,
    /// 一行超过 16 MiB。
    TooLong,
    Read,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Exit {
    pub(crate) code: Option<i32>,
    /// 被信号结束。
    pub(crate) signaled: bool,
}

impl Exit {
    fn of(status: &io::Result<ExitStatus>) -> Exit {
        match status {
            Ok(status) => Exit {
                code: status.code(),
                signaled: status.code().is_none(),
            },
            Err(_) => Exit {
                code: None,
                signaled: false,
            },
        }
    }
}

struct Outbox {
    lines: VecDeque<Vec<u8>>,
    /// 有一个调用者正在排空队列。
    draining: bool,
    closing: bool,
}

/// 一个正在运行（或刚退出）的 ACP 进程。
pub(crate) struct Conn {
    /// 引擎内唯一。
    pub(crate) id: u64,
    pub(crate) pid: u32,
    started: Instant,
    outbox: Mutex<Outbox>,
    stdin: Mutex<Option<ChildStdin>>,
    exited: Mutex<bool>,
    exit_changed: Condvar,
    stderr_bytes: AtomicU64,
}

/// [`Conn::launch`] 的结果：连接，和要交给读线程的几个句柄。
pub(crate) struct Launched {
    pub(crate) conn: Arc<Conn>,
    pub(crate) child: Child,
    pub(crate) stdout: ChildStdout,
    pub(crate) stderr: ChildStderr,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

impl Conn {
    /// 启动进程：自己的进程组（`process_group(0)`，组 ID 就是 pid），环境只有 `spec.env`，
    /// 三条管道。阻塞（fork/exec），只在 executor 上调用。
    pub(crate) fn launch(spec: &Spec, id: u64) -> io::Result<Launched> {
        let mut command = Command::new(&spec.program);
        command
            .current_dir(&spec.cwd)
            .env_clear()
            .envs(spec.env.iter().map(|(name, value)| (name, value)))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
        let mut child = command.spawn()?;
        let (Some(stdin), Some(stdout), Some(stderr)) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(io::Error::other("missing pipe"));
        };
        let conn = Arc::new(Conn {
            id,
            pid: child.id(),
            started: Instant::now(),
            outbox: Mutex::new(Outbox {
                lines: VecDeque::new(),
                draining: false,
                closing: false,
            }),
            stdin: Mutex::new(Some(stdin)),
            exited: Mutex::new(false),
            exit_changed: Condvar::new(),
            stderr_bytes: AtomicU64::new(0),
        });
        Ok(Launched {
            conn,
            child,
            stdout,
            stderr,
        })
    }

    /// 把一行放进发送队列。返回 true：没有人在排空，调用者要安排一次 [`drain`](Self::drain)。
    /// 关闭之后放进来的行丢弃（返回 false）。
    pub(crate) fn push(&self, line: Vec<u8>) -> bool {
        let mut outbox = lock(&self.outbox);
        if outbox.closing {
            return false;
        }
        outbox.lines.push_back(line);
        !std::mem::replace(&mut outbox.draining, true)
    }

    /// 排空发送队列。阻塞（写管道），只在 executor 上调用。写失败（对端已关）时丢掉队列里剩下的。
    /// 队列排空之后，如果已经在关闭，就关闭 stdin。
    pub(crate) fn drain(&self) {
        loop {
            let line = {
                let mut outbox = lock(&self.outbox);
                match outbox.lines.pop_front() {
                    Some(line) => line,
                    None => {
                        outbox.draining = false;
                        if outbox.closing {
                            drop(outbox);
                            *lock(&self.stdin) = None;
                        }
                        return;
                    }
                }
            };
            if self.write(&line).is_err() {
                let mut outbox = lock(&self.outbox);
                outbox.lines.clear();
            }
        }
    }

    fn write(&self, line: &[u8]) -> io::Result<()> {
        let mut stdin = lock(&self.stdin);
        let Some(pipe) = stdin.as_mut() else {
            return Err(io::ErrorKind::BrokenPipe.into());
        };
        pipe.write_all(line)?;
        pipe.flush()
    }

    /// 关闭：队列里剩下的行写完，关闭 stdin，向进程组发 SIGTERM（设计：关闭 stdin、SIGTERM，
    /// 2 秒后 SIGKILL，后者由引擎的计时器调用 [`kill`](Self::kill)）。不等进程退出。
    /// 有排空的调用者在写（对端不读）时不等它，直接 SIGTERM：进程退出后写会失败。
    pub(crate) fn close(&self) {
        let rest = {
            let mut outbox = lock(&self.outbox);
            outbox.closing = true;
            if outbox.draining {
                None
            } else {
                // 登记为排空者，别人的 `push` 不再安排任务。
                outbox.draining = true;
                Some(std::mem::take(&mut outbox.lines))
            }
        };
        if let Some(lines) = rest {
            for line in lines {
                if self.write(&line).is_err() {
                    break;
                }
            }
            lock(&self.outbox).draining = false;
            if let Ok(mut stdin) = self.stdin.try_lock() {
                *stdin = None;
            }
        }
        self.signal(Signal::TERM);
    }

    /// SIGKILL 给进程组（进程还没被回收时）。
    pub(crate) fn kill(&self) {
        self.signal(Signal::KILL);
    }

    fn signal(&self, signal: Signal) {
        let exited = lock(&self.exited);
        if *exited {
            return;
        }
        if let Some(pid) = i32::try_from(self.pid).ok().and_then(Pid::from_raw) {
            // 组可能已经不在了。
            let _ = kill_process_group(pid, signal);
        }
    }

    /// 进程是否已被回收。
    pub(crate) fn has_exited(&self) -> bool {
        *lock(&self.exited)
    }

    /// 等进程被回收，至多 `timeout`。返回是否已回收。
    pub(crate) fn wait_exited(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let mut exited = lock(&self.exited);
        while !*exited {
            let Some(left) = deadline.checked_duration_since(Instant::now()) else {
                return false;
            };
            exited = self
                .exit_changed
                .wait_timeout(exited, left)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
        true
    }

    /// 进程回收之后：杀掉组里残留的进程（适配器可能留下孙进程），再唤醒等待的人。
    fn mark_exited(&self) {
        let mut exited = lock(&self.exited);
        if let Some(pid) = i32::try_from(self.pid).ok().and_then(Pid::from_raw) {
            let _ = kill_process_group(pid, Signal::KILL);
        }
        *exited = true;
        self.exit_changed.notify_all();
    }

    /// stderr 读掉的字节数。
    pub(crate) fn stderr_bytes(&self) -> u64 {
        self.stderr_bytes.load(Ordering::Relaxed)
    }

    /// 启动至今的毫秒数。
    pub(crate) fn runtime_ms(&self) -> u64 {
        u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX)
    }
}

/// stderr 只数字节，不保留内容。读到对端关闭为止。
pub(crate) fn drain_stderr(conn: &Conn, mut stderr: ChildStderr) {
    let mut buf = [0u8; 8192];
    loop {
        match stderr.read(&mut buf) {
            Ok(0) => return,
            Ok(n) => {
                conn.stderr_bytes.fetch_add(n as u64, Ordering::Relaxed);
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => return,
        }
    }
}

/// 读线程的主体：逐行读 stdout 交给 `sink`，结束后回收进程。
pub(crate) fn read_loop(conn: Arc<Conn>, mut child: Child, stdout: ChildStdout, sink: &dyn Sink) {
    let mut reader = BufReader::with_capacity(64 << 10, stdout);
    let mut buf = Vec::new();
    let end = loop {
        match read_line(&mut reader, &mut buf, MAX_LINE) {
            Ok(Line::Ready) => {
                if buf.last() == Some(&b'\r') {
                    buf.pop();
                }
                if !buf.is_empty() {
                    sink.line(&conn, &buf);
                }
                // 一条很大的消息之后不留着它的缓冲。
                if buf.capacity() > (1 << 20) {
                    buf = Vec::new();
                }
            }
            Ok(Line::Eof) => break End::Eof,
            Ok(Line::TooLong) => break End::TooLong,
            Err(_) => break End::Read,
        }
    };
    drop(buf);
    sink.ended(&conn, end);
    let status = child.wait();
    conn.mark_exited();
    sink.exited(&conn, Exit::of(&status));
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Line {
    Eof,
    Ready,
    TooLong,
}

/// 读一行到 `buf`（不含换行）。一行超过 `max` 字节时返回 `TooLong`，不再读。
fn read_line(reader: &mut impl BufRead, buf: &mut Vec<u8>, max: usize) -> io::Result<Line> {
    buf.clear();
    loop {
        let chunk = match reader.fill_buf() {
            Ok(chunk) => chunk,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        if chunk.is_empty() {
            // 没有换行结尾的最后一行也算一行。
            return Ok(if buf.is_empty() {
                Line::Eof
            } else {
                Line::Ready
            });
        }
        match chunk.iter().position(|&b| b == b'\n') {
            Some(at) => {
                if buf.len() + at > max {
                    return Ok(Line::TooLong);
                }
                buf.extend_from_slice(&chunk[..at]);
                reader.consume(at + 1);
                return Ok(Line::Ready);
            }
            None => {
                let n = chunk.len();
                if buf.len() + n > max {
                    return Ok(Line::TooLong);
                }
                buf.extend_from_slice(chunk);
                reader.consume(n);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn lines(input: &[u8], max: usize) -> Vec<(Line, Vec<u8>)> {
        let mut reader = BufReader::with_capacity(4, Cursor::new(input.to_vec()));
        let mut buf = Vec::new();
        let mut out = Vec::new();
        loop {
            let line = read_line(&mut reader, &mut buf, max).unwrap();
            out.push((line, buf.clone()));
            if matches!(line, Line::Eof | Line::TooLong) {
                return out;
            }
        }
    }

    #[test]
    fn lines_are_split_across_small_reads() {
        let got = lines(b"one\ntwo three\n\nlast", 100);
        let text: Vec<_> = got
            .iter()
            .map(|(line, buf)| (*line, String::from_utf8_lossy(buf).into_owned()))
            .collect();
        assert_eq!(
            text,
            [
                (Line::Ready, "one".into()),
                (Line::Ready, "two three".into()),
                (Line::Ready, String::new()),
                (Line::Ready, "last".into()),
                (Line::Eof, String::new()),
            ]
        );
    }

    #[test]
    fn a_line_over_the_limit_stops_the_reading() {
        // Exactly the limit is fine; one more byte is not, with or without a newline.
        assert_eq!(lines(b"12345\n", 5)[0].0, Line::Ready);
        assert_eq!(lines(b"123456\n", 5).last().unwrap().0, Line::TooLong);
        assert_eq!(lines(b"1234567890", 5).last().unwrap().0, Line::TooLong);
        assert_eq!(lines(b"ok\n1234567890", 5).last().unwrap().0, Line::TooLong);
    }

    #[test]
    fn exits_report_code_or_signal() {
        use std::os::unix::process::ExitStatusExt;
        let exit = Exit::of(&Ok(ExitStatus::from_raw(3 << 8)));
        assert_eq!(
            exit,
            Exit {
                code: Some(3),
                signaled: false
            }
        );
        let exit = Exit::of(&Ok(ExitStatus::from_raw(15)));
        assert_eq!(
            exit,
            Exit {
                code: None,
                signaled: true
            }
        );
    }
}
