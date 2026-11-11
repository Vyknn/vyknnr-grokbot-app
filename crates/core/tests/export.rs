use assistant_core::export::*;
use assistant_core::store::*;
use assistant_core::{NewMessage, ProviderId, Role};
use serde_json::json;
use std::path::Path;

fn nm(role: Role, p: Option<ProviderId>, c: &str) -> NewMessage {
    NewMessage {
        role,
        provider: p,
        content: c.into(),
    }
}

fn seed(user: &str, cmd: &str, out: &str) -> (Store, i64) {
    let s = Store::open_in_memory().unwrap();
    let c = s.create_conversation("แชต", Path::new(r"C:\งาน")).unwrap();
    let m = s.append_message(c.id, nm(Role::User, None, user)).unwrap();
    s.append_message(
        c.id,
        nm(Role::Assistant, Some(ProviderId::Hermes), "ตอบกลับ"),
    )
    .unwrap();
    s.append_message(c.id, nm(Role::CommandResult, None, "ok"))
        .unwrap();
    s.record_decision(m.id, DecisionKind::Route, &json!({"why":"x"}))
        .unwrap();
    let id = s
        .insert_command(
            c.id,
            cmd,
            Path::new(r"C:\งาน"),
            &json!({}),
            Approval::UserEdited,
        )
        .unwrap();
    s.finish_command(id, Some(0), out, false).unwrap();
    (s, c.id)
}

#[test]
fn markdown_contains_thai_and_provider_labels() {
    let (s, id) = seed("สวัสดี", "dir", "files");
    let md = export_markdown(&s, id, &[]).unwrap();
    assert!(md.starts_with("# แชต"));
    for t in [
        "สวัสดี",
        "## ผู้ใช้",
        "## ผู้ช่วย (hermes)",
        "## ผลคำสั่ง",
        "## คำสั่ง",
        "```powershell",
        "exit: 0",
        "การอนุมัติ: user_edited",
    ] {
        assert!(md.contains(t), "missing {t}");
    }
}

#[test]
fn markdown_fence_grows_when_content_has_backticks() {
    let (s, id) = seed("a ``` b", "echo ```", "o");
    let md = export_markdown(&s, id, &[]).unwrap();
    assert!(md.contains("````powershell"));
}

#[test]
fn json_is_valid_and_complete() {
    let (s, id) = seed("สวัสดี", "dir", "files");
    let v: serde_json::Value = serde_json::from_str(&export_json(&s, id, &[]).unwrap()).unwrap();
    assert_eq!(v["messages"].as_array().unwrap().len(), 3);
    assert_eq!(v["commands"].as_array().unwrap().len(), 1);
    assert_eq!(v["decisions"].as_array().unwrap().len(), 1);
    assert_eq!(v["conversation"]["title"], "แชต");
}

#[test]
fn export_never_contains_secrets() {
    let (s, id) = seed(
        "key sk-ant-api03-XXXXXXXX",
        "$env:TYPESAFE_API_KEY='ts_live_YYYYYYYY'",
        "Bearer abcdef123456",
    );
    for out in [
        export_markdown(&s, id, &[]).unwrap(),
        export_json(&s, id, &[]).unwrap(),
    ] {
        assert!(!out.contains("sk-ant-api03-XXXXXXXX"));
        assert!(!out.contains("ts_live_YYYYYYYY"));
        assert!(!out.contains("abcdef123456"));
        assert!(out.contains("[REDACTED]"));
    }
}

#[test]
fn known_secret_redacted_without_regex_match() {
    let (s, id) = seed("my pw is hunter2hunter2", "dir", "o");
    let k = vec!["hunter2hunter2".to_string()];
    assert!(!export_markdown(&s, id, &k)
        .unwrap()
        .contains("hunter2hunter2"));
    assert!(!export_json(&s, id, &k).unwrap().contains("hunter2hunter2"));
    assert_eq!(redact_secrets("a hunter2hunter2 b", &k), "a [REDACTED] b");
}

#[test]
fn short_or_empty_known_secrets_ignored() {
    let k = vec![String::new(), "abc".to_string()];
    assert_eq!(redact_secrets("abc def", &k), "abc def");
}

#[test]
fn decision_payload_strings_redacted() {
    let s = Store::open_in_memory().unwrap();
    let c = s.create_conversation("t", Path::new("C:\\")).unwrap();
    let m = s.append_message(c.id, nm(Role::User, None, "hi")).unwrap();
    s.record_decision(
        m.id,
        DecisionKind::Command,
        &json!({"n":["Bearer abcdef123456"]}),
    )
    .unwrap();
    assert!(!export_json(&s, c.id, &[]).unwrap().contains("abcdef123456"));
}

#[test]
fn missing_conversation_is_not_found() {
    let s = Store::open_in_memory().unwrap();
    assert!(matches!(
        export_json(&s, 5, &[]),
        Err(ExportError::Store(StoreError::NotFound))
    ));
    assert!(matches!(
        export_markdown(&s, 5, &[]),
        Err(ExportError::Store(StoreError::NotFound))
    ));
}

#[test]
fn writes_to_path_with_spaces_and_thai() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("แชต ทดสอบ").join("a b.md");
    write_export(&p, "สวัสดี").unwrap();
    let b = std::fs::read(&p).unwrap();
    assert_eq!(b, "สวัสดี".as_bytes());
    assert_ne!(&b[..3], &[0xEF, 0xBB, 0xBF]);
}
