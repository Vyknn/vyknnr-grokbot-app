#![cfg(windows)]
use assistant_core::runner::*;
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

fn tmp() -> std::path::PathBuf {
    std::env::temp_dir()
}

#[tokio::test]
async fn captures_thai_output_and_exit_code() {
    let mut chunks = Vec::new();
    let r = run_powershell(
        "Write-Output 'สวัสดี'; exit 3",
        &tmp(),
        &RunLimits::default(),
        |c| chunks.push(c),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(r.output.trim(), "สวัสดี");
    assert_eq!(r.exit_code, Some(3));
    assert!(!r.timed_out && !r.cancelled && !r.truncated);
    assert!(chunks.iter().any(|c| matches!(c, RunChunk::Stdout(s) if s.contains("สวัสดี"))));
}

#[tokio::test]
async fn captures_stderr() {
    let mut chunks = Vec::new();
    let r = run_powershell(
        "[Console]::Error.WriteLine('oops')",
        &tmp(),
        &RunLimits::default(),
        |c| chunks.push(c),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert!(r.output.contains("oops"));
    assert!(chunks.iter().any(|c| matches!(c, RunChunk::Stderr(s) if s.contains("oops"))));
}

#[tokio::test]
async fn runs_in_cwd_with_spaces() {
    let d = tempfile::tempdir().unwrap();
    let cwd = d.path().join("A B");
    std::fs::create_dir(&cwd).unwrap();
    let r = run_powershell(
        "(Get-Location).Path",
        &cwd,
        &RunLimits::default(),
        |_| {},
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert!(r.output.trim().ends_with("A B"), "{}", r.output);
}

#[tokio::test]
async fn missing_cwd_is_not_found() {
    let e = run_powershell(
        "1",
        std::path::Path::new(r"Z:/no/such/dir_xyz"),
        &RunLimits::default(),
        |_| {},
        CancellationToken::new(),
    )
    .await
    .unwrap_err();
    assert_eq!(e.kind(), std::io::ErrorKind::NotFound);
}

#[tokio::test]
async fn timeout_kills_process_tree() {
    let d = tempfile::tempdir().unwrap();
    let pidfile = d.path().join("pid.txt");
    let cmd = format!(
        "$p = Start-Process powershell -ArgumentList '-NoProfile','-Command','Start-Sleep 60' -PassThru -WindowStyle Hidden; \
         $p.Id | Set-Content -Path '{}'; Start-Sleep 30",
        pidfile.display()
    );
    let limits = RunLimits { timeout: Duration::from_secs(3), ..Default::default() };
    let t = Instant::now();
    let r = run_powershell(&cmd, &tmp(), &limits, |_| {}, CancellationToken::new())
        .await
        .unwrap();
    assert!(r.timed_out);
    assert_eq!(r.exit_code, None);
    assert!(t.elapsed() < Duration::from_secs(10), "{:?}", t.elapsed());
    let pid: u32 = std::fs::read_to_string(&pidfile).unwrap().trim().parse().unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    let st = std::process::Command::new("powershell")
        .args(["-NoProfile", "-Command", &format!("Get-Process -Id {pid} -ErrorAction Stop")])
        .output()
        .unwrap();
    assert!(!st.status.success(), "child {pid} still alive");
}

#[tokio::test]
async fn cancel_stops() {
    let c = CancellationToken::new();
    let c2 = c.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(1500)).await;
        c2.cancel();
    });
    let t = Instant::now();
    let r = run_powershell("Start-Sleep 30", &tmp(), &RunLimits::default(), |_| {}, c)
        .await
        .unwrap();
    assert!(r.cancelled && !r.timed_out);
    assert_eq!(r.exit_code, None);
    assert!(t.elapsed() < Duration::from_secs(8));
}

#[tokio::test]
async fn large_output_truncated() {
    let mut streamed = 0usize;
    let r = run_powershell(
        "1..200000 | % { 'xxxxxxxx' }",
        &tmp(),
        &RunLimits::default(),
        |c| {
            if let RunChunk::Stdout(s) = c {
                streamed += s.len();
            }
        },
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert!(r.output.len() <= 64 * 1024);
    assert!(r.truncated);
    assert!(streamed > 64 * 1024);
    assert_eq!(r.exit_code, Some(0));
}

fn res(output: String, truncated: bool, timed_out: bool, cancelled: bool) -> RunResult {
    RunResult { exit_code: None, output, truncated, timed_out, cancelled }
}

#[test]
fn for_model_cuts_on_char_boundary() {
    let r = res("ก".repeat(10_000), false, false, false);
    let s = for_model(&r, 100);
    assert!(s.ends_with("ไบต์]"));
    assert!(s.contains("[ตัดเหลือ 99 จาก 30000 ไบต์]"), "{s}");
}

#[test]
fn for_model_unchanged_when_small_and_flags() {
    let r = res("hi".into(), false, false, false);
    assert_eq!(for_model(&r, 100), "hi");
    let r = res("hi".into(), false, true, true);
    assert_eq!(for_model(&r, 100), "hi\n[หมดเวลา]\n[ถูกยกเลิก]");
}

#[test]
fn for_model_stored_truncated_marker() {
    let r = res("abc".into(), true, false, false);
    let s = for_model(&r, 100);
    assert!(s.contains("(เก็บไว้ไม่ครบ)"), "{s}");
    assert!(s.contains("[ตัดเหลือ 3 จาก 3 ไบต์"), "{s}");
}
