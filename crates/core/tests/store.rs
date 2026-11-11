use assistant_core::store::*;
use assistant_core::{NewMessage, ProviderId, Role};
use serde_json::json;
use std::path::Path;

fn um(c: &str) -> NewMessage {
    NewMessage {
        role: Role::User,
        provider: None,
        content: c.into(),
    }
}

#[test]
fn thai_text_round_trips() {
    let s = Store::open_in_memory().unwrap();
    let c = s
        .create_conversation("แชต", Path::new(r"C:\งาน ใหม่"))
        .unwrap();
    let m = s
        .append_message(
            c.id,
            NewMessage {
                role: Role::Assistant,
                provider: Some(ProviderId::ClaudeCli),
                content: "สวัสดี ครับ 🙂".into(),
            },
        )
        .unwrap();
    let got = s.messages(c.id).unwrap();
    assert_eq!(got[0].content, "สวัสดี ครับ 🙂");
    assert_eq!(got[0], m);
    assert_eq!(got[0].provider, Some(ProviderId::ClaudeCli));
    assert_eq!(
        s.list_conversations().unwrap()[0].cwd,
        Path::new(r"C:\งาน ใหม่")
    );
}

#[test]
fn list_orders_by_last_activity() {
    let s = Store::open_in_memory().unwrap();
    let a = s.create_conversation("a", Path::new(".")).unwrap();
    let b = s.create_conversation("b", Path::new(".")).unwrap();
    assert_eq!(s.list_conversations().unwrap()[0].id, b.id);
    std::thread::sleep(std::time::Duration::from_millis(5));
    s.append_message(a.id, um("hi")).unwrap();
    let l = s.list_conversations().unwrap();
    assert_eq!(l[0].id, a.id);
    assert_eq!(l[1].id, b.id);
}

#[test]
fn search_matches_title_and_content_case_insensitive() {
    let s = Store::open_in_memory().unwrap();
    let a = s.create_conversation("Deploy", Path::new(".")).unwrap();
    let b = s.create_conversation("other", Path::new(".")).unwrap();
    s.append_message(b.id, um("run kubectl now")).unwrap();
    s.append_message(b.id, um("kubectl again")).unwrap();
    assert_eq!(
        s.search("deploy")
            .unwrap()
            .iter()
            .map(|c| c.id)
            .collect::<Vec<_>>(),
        vec![a.id]
    );
    assert_eq!(
        s.search("KUBECTL")
            .unwrap()
            .iter()
            .map(|c| c.id)
            .collect::<Vec<_>>(),
        vec![b.id]
    );
    assert!(s.search("nothing").unwrap().is_empty());
}

#[test]
fn search_escapes_wildcards() {
    let s = Store::open_in_memory().unwrap();
    let a = s.create_conversation("x", Path::new(".")).unwrap();
    let b = s.create_conversation("y", Path::new(".")).unwrap();
    s.append_message(a.id, um("500 items")).unwrap();
    s.append_message(b.id, um("50% done")).unwrap();
    assert_eq!(
        s.search("50%")
            .unwrap()
            .iter()
            .map(|c| c.id)
            .collect::<Vec<_>>(),
        vec![b.id]
    );
    assert!(s.search("5_0").unwrap().is_empty());
    assert!(s.search("\\").unwrap().is_empty());
}

#[test]
fn delete_cascades() {
    let s = Store::open_in_memory().unwrap();
    let c = s.create_conversation("t", Path::new(".")).unwrap();
    let m = s.append_message(c.id, um("x")).unwrap();
    s.record_decision(m.id, DecisionKind::Route, &json!({"a":1}))
        .unwrap();
    let id = s
        .insert_command(c.id, "ls", Path::new("."), &json!({}), Approval::Auto)
        .unwrap();
    s.finish_command(id, Some(0), "out", false).unwrap();
    s.delete_conversation(c.id).unwrap();
    assert!(s.messages(c.id).unwrap().is_empty());
    assert!(s.commands(c.id).unwrap().is_empty());
    assert!(s.decisions(c.id).unwrap().is_empty());
}

#[test]
fn reopen_keeps_data() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("a.db");
    let id = {
        let s = Store::open(&p).unwrap();
        let c = s.create_conversation("t", Path::new(".")).unwrap();
        s.append_message(c.id, um("keep")).unwrap();
        c.id
    };
    let s = Store::open(&p).unwrap();
    assert_eq!(s.messages(id).unwrap()[0].content, "keep");
}

#[test]
fn rename_and_not_found() {
    let s = Store::open_in_memory().unwrap();
    let c = s.create_conversation("t", Path::new(".")).unwrap();
    s.rename_conversation(c.id, "new").unwrap();
    assert_eq!(s.list_conversations().unwrap()[0].title, "new");
    assert!(matches!(
        s.rename_conversation(999, "x"),
        Err(StoreError::NotFound)
    ));
    assert!(matches!(
        s.append_message(999, um("x")),
        Err(StoreError::NotFound)
    ));
}

#[test]
fn decisions_in_order() {
    let s = Store::open_in_memory().unwrap();
    let c = s.create_conversation("t", Path::new(".")).unwrap();
    let other = s.create_conversation("o", Path::new(".")).unwrap();
    let m = s.append_message(c.id, um("x")).unwrap();
    let mo = s.append_message(other.id, um("y")).unwrap();
    let d1 = s
        .record_decision(m.id, DecisionKind::Route, &json!({"n":1}))
        .unwrap();
    s.record_decision(mo.id, DecisionKind::Command, &json!({}))
        .unwrap();
    let d2 = s
        .record_decision(m.id, DecisionKind::Command, &json!({"n":2}))
        .unwrap();
    let ds = s.decisions(c.id).unwrap();
    assert_eq!(ds.iter().map(|d| d.id).collect::<Vec<_>>(), vec![d1, d2]);
    assert_eq!(ds[0].kind, DecisionKind::Route);
    assert_eq!(ds[1].payload, json!({"n":2}));
    assert_eq!(ds[0].message_id, m.id);
}

#[test]
fn command_lifecycle() {
    let s = Store::open_in_memory().unwrap();
    let c = s.create_conversation("t", Path::new(".")).unwrap();
    let id = s
        .insert_command(
            c.id,
            "rm x",
            Path::new(r"C:\a b"),
            &json!({"risk":"high"}),
            Approval::UserEdited,
        )
        .unwrap();
    let r = &s.commands(c.id).unwrap()[0];
    assert_eq!(r.id, id);
    assert_eq!(r.approval, Approval::UserEdited);
    assert_eq!(r.exit_code, None);
    assert_eq!(r.output, "");
    assert!(!r.truncated);
    assert!(r.finished_at.is_none());
    assert_eq!(r.check, json!({"risk":"high"}));
    s.finish_command(id, Some(2), "boom", true).unwrap();
    let r = &s.commands(c.id).unwrap()[0];
    assert_eq!(r.exit_code, Some(2));
    assert_eq!(r.output, "boom");
    assert!(r.truncated);
    assert!(r.finished_at.is_some());
    let d = s
        .insert_command(c.id, "x", Path::new("."), &json!({}), Approval::Denied)
        .unwrap();
    assert!(s
        .commands(c.id)
        .unwrap()
        .iter()
        .any(|r| r.id == d && r.finished_at.is_none()));
    assert!(matches!(
        s.finish_command(9999, None, "", false),
        Err(StoreError::NotFound)
    ));
}

#[test]
fn store_is_send_sync() {
    fn f<T: Send + Sync>() {}
    f::<Store>();
}

#[test]
fn conversation_by_id_and_not_found() {
    let s = Store::open_in_memory().unwrap();
    let c = s.create_conversation("หัวข้อ", Path::new("C:/x")).unwrap();
    assert_eq!(s.conversation(c.id).unwrap(), c);
    assert!(matches!(s.conversation(999), Err(StoreError::NotFound)));
}
