//! PowerShell command runner: streaming output, timeout, cancel, truncation.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone)]
pub struct RunLimits {
    pub timeout: Duration,
    pub max_stored: usize,
    pub max_for_model: usize,
}

impl Default for RunLimits {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(120),
            max_stored: 64 * 1024,
            max_for_model: 16 * 1024,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RunChunk {
    Stdout(String),
    Stderr(String),
}

#[derive(Debug, Clone)]
pub struct RunResult {
    pub exit_code: Option<i32>,
    pub output: String,
    pub truncated: bool,
    pub timed_out: bool,
    pub cancelled: bool,
}

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const PREFIX: &str =
    "[Console]::OutputEncoding=[Text.Encoding]::UTF8; $OutputEncoding=[Text.Encoding]::UTF8; ";

/// Decode as much of `pending` as possible; keeps an incomplete trailing sequence.
fn drain_utf8(pending: &mut Vec<u8>, eof: bool) -> String {
    let mut out = String::new();
    loop {
        match std::str::from_utf8(pending) {
            Ok(s) => {
                out.push_str(s);
                pending.clear();
                break;
            }
            Err(e) => {
                let valid = e.valid_up_to();
                out.push_str(std::str::from_utf8(&pending[..valid]).unwrap());
                match e.error_len() {
                    Some(n) => {
                        out.push('\u{FFFD}');
                        pending.drain(..valid + n);
                    }
                    None => {
                        pending.drain(..valid);
                        if eof {
                            out.push('\u{FFFD}');
                            pending.clear();
                        }
                        break;
                    }
                }
            }
        }
    }
    out
}

async fn pump<R: AsyncRead + Unpin>(mut r: R, stderr: bool, tx: mpsc::Sender<RunChunk>) {
    let mut buf = [0u8; 8192];
    let mut pending: Vec<u8> = Vec::new();
    let wrap = |s: String| {
        if stderr {
            RunChunk::Stderr(s)
        } else {
            RunChunk::Stdout(s)
        }
    };
    loop {
        match r.read(&mut buf).await {
            Ok(0) | Err(_) => {
                let s = drain_utf8(&mut pending, true);
                if !s.is_empty() {
                    let _ = tx.send(wrap(s)).await;
                }
                return;
            }
            Ok(n) => {
                pending.extend_from_slice(&buf[..n]);
                let s = drain_utf8(&mut pending, false);
                if !s.is_empty() && tx.send(wrap(s)).await.is_err() {
                    return;
                }
            }
        }
    }
}

fn append_capped(out: &mut String, s: &str, max: usize, truncated: &mut bool) {
    if *truncated {
        return;
    }
    let room = max.saturating_sub(out.len());
    if s.len() <= room {
        out.push_str(s);
    } else {
        let mut cut = room;
        while !s.is_char_boundary(cut) {
            cut -= 1;
        }
        out.push_str(&s[..cut]);
        *truncated = true;
    }
}

async fn kill_tree(pid: Option<u32>) {
    if let Some(pid) = pid {
        let _ = Command::new("taskkill")
            .args(["/T", "/F", "/PID", &pid.to_string()])
            .creation_flags(CREATE_NO_WINDOW)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await;
    }
}

pub async fn run_powershell(
    command: &str,
    cwd: &Path,
    limits: &RunLimits,
    mut on_chunk: impl FnMut(RunChunk) + Send,
    cancel: CancellationToken,
) -> std::io::Result<RunResult> {
    if !cwd.is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("cwd not found: {}", cwd.display()),
        ));
    }
    let mut child = Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command"])
        .arg(format!("{PREFIX}{command}"))
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(CREATE_NO_WINDOW)
        .kill_on_drop(true)
        .spawn()?;
    let pid = child.id();

    let (tx, mut rx) = mpsc::channel::<RunChunk>(64);
    let h_out = tokio::spawn(pump(child.stdout.take().unwrap(), false, tx.clone()));
    let h_err = tokio::spawn(pump(child.stderr.take().unwrap(), true, tx));

    let mut output = String::new();
    let mut truncated = false;
    let mut handle = |c: RunChunk, output: &mut String, truncated: &mut bool| {
        let s = match &c {
            RunChunk::Stdout(s) | RunChunk::Stderr(s) => s.clone(),
        };
        append_capped(output, &s, limits.max_stored, truncated);
        on_chunk(c);
    };

    let deadline = tokio::time::sleep(limits.timeout);
    tokio::pin!(deadline);
    let mut status = None;
    let mut timed_out = false;
    let mut cancelled = false;
    let mut channel_open = true;

    while status.is_none() && !timed_out && !cancelled {
        tokio::select! {
            m = rx.recv(), if channel_open => match m {
                Some(c) => handle(c, &mut output, &mut truncated),
                None => channel_open = false,
            },
            s = child.wait() => status = Some(s?),
            _ = &mut deadline => timed_out = true,
            _ = cancel.cancelled() => cancelled = true,
        }
    }

    if timed_out || cancelled {
        kill_tree(pid).await;
        let _ = child.wait().await;
    }

    // Drain remaining output; bounded in case a surviving grandchild holds the pipes.
    let drain = async {
        while let Some(c) = rx.recv().await {
            handle(c, &mut output, &mut truncated);
        }
    };
    let _ = tokio::time::timeout(Duration::from_secs(2), drain).await;
    h_out.abort();
    h_err.abort();

    let exit_code = if timed_out || cancelled {
        None
    } else {
        status.and_then(|s| s.code())
    };
    Ok(RunResult {
        exit_code,
        output,
        truncated,
        timed_out,
        cancelled,
    })
}

fn cut_boundary(s: &str, max: usize) -> usize {
    let mut n = max.min(s.len());
    while !s.is_char_boundary(n) {
        n -= 1;
    }
    n
}

pub fn for_model(r: &RunResult, max: usize) -> String {
    let m = r.output.len();
    let mut s = if m <= max && !r.truncated {
        r.output.clone()
    } else {
        let n = cut_boundary(&r.output, max);
        let mut s = r.output[..n].to_string();
        s.push_str(&format!("\n[ตัดเหลือ {n} จาก {m} ไบต์]"));
        if r.truncated {
            s.push_str(" (เก็บไว้ไม่ครบ)");
        }
        s
    };
    if r.timed_out {
        s.push_str("\n[หมดเวลา]");
    }
    if r.cancelled {
        s.push_str("\n[ถูกยกเลิก]");
    }
    s
}
