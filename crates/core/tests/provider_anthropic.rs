use assistant_core::providers::anthropic::{AnthropicProvider, SseParser};
use assistant_core::providers::{ChatProvider, ProviderTurn};
use assistant_core::{Availability, ChatEvent, Message, ProviderId, Role, TurnRequest};
use serde_json::{json, Value};
use url::Url;
use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn sse(events: &[(&str, Value)]) -> String {
    events
        .iter()
        .map(|(e, d)| format!("event: {e}\ndata: {d}\n\n"))
        .collect()
}

fn text_delta(i: usize, t: &str) -> (&'static str, Value) {
    (
        "content_block_delta",
        json!({"type":"content_block_delta","index":i,"delta":{"type":"text_delta","text":t}}),
    )
}

fn msg_delta(reason: &str) -> (&'static str, Value) {
    (
        "message_delta",
        json!({"type":"message_delta","delta":{"stop_reason":reason}}),
    )
}

fn stop() -> (&'static str, Value) {
    ("message_stop", json!({"type":"message_stop"}))
}

fn tool_start(i: usize, id: &str, name: &str) -> (&'static str, Value) {
    (
        "content_block_start",
        json!({"type":"content_block_start","index":i,"content_block":{"type":"tool_use","id":id,"name":name,"input":{}}}),
    )
}

fn json_delta(i: usize, p: &str) -> (&'static str, Value) {
    (
        "content_block_delta",
        json!({"type":"content_block_delta","index":i,"delta":{"type":"input_json_delta","partial_json":p}}),
    )
}

fn block_stop(i: usize) -> (&'static str, Value) {
    (
        "content_block_stop",
        json!({"type":"content_block_stop","index":i}),
    )
}

fn event_stream(body: String) -> ResponseTemplate {
    ResponseTemplate::new(200)
        .insert_header("content-type", "text/event-stream")
        .set_body_string(body)
}

fn req() -> TurnRequest {
    TurnRequest {
        conversation_id: 1,
        turn_token: "t".into(),
        history: vec![Message {
            id: 1,
            conversation_id: 1,
            role: Role::User,
            provider: None,
            content: "hi".into(),
            created_at: chrono::Utc::now(),
        }],
        cwd: std::env::temp_dir(),
    }
}

fn provider(server: &MockServer) -> AnthropicProvider {
    AnthropicProvider::new(
        Url::parse(&server.uri()).unwrap(),
        Some("sk-secret".into()),
        "claude-test".into(),
    )
}

async fn drain(turn: &mut Box<dyn ProviderTurn>) -> Vec<ChatEvent> {
    let mut v = vec![];
    while let Some(e) = turn.next().await {
        v.push(e);
    }
    v
}

#[tokio::test]
async fn streams_text_then_done() {
    let server = MockServer::start().await;
    let body = sse(&[
        ("ping", json!({"type":"ping"})),
        text_delta(0, "สวัส"),
        text_delta(0, "ดี"),
        msg_delta("end_turn"),
        stop(),
    ]);
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(event_stream(body))
        .mount(&server)
        .await;
    let p = provider(&server);
    assert_eq!(p.id(), ProviderId::Api);
    let mut turn = p.start(req()).await.unwrap();
    assert_eq!(
        drain(&mut turn).await,
        vec![
            ChatEvent::TextDelta("สวัส".into()),
            ChatEvent::TextDelta("ดี".into()),
            ChatEvent::Done
        ]
    );
    let reqs = server.received_requests().await.unwrap();
    let h = &reqs[0].headers;
    assert_eq!(h.get("x-api-key").unwrap(), "sk-secret");
    assert_eq!(h.get("anthropic-version").unwrap(), "2023-06-01");
    let b: Value = reqs[0].body_json().unwrap();
    assert_eq!(b["max_tokens"], 8192);
    assert_eq!(b["stream"], true);
    assert_eq!(b["model"], "claude-test");
    assert_eq!(b["tools"][0]["name"], "run_command");
}

async fn mount_two_rounds(server: &MockServer, first: String, second: String) {
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(body_string_contains("tool_result"))
        .respond_with(event_stream(second))
        .with_priority(1)
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(event_stream(first))
        .with_priority(5)
        .mount(server)
        .await;
}

#[tokio::test]
async fn tool_use_round_trip() {
    let server = MockServer::start().await;
    let first = sse(&[
        text_delta(0, "ok"),
        block_stop(0),
        tool_start(1, "toolu_1", "run_command"),
        json_delta(1, "{\"comm"),
        json_delta(1, "and\":\"dir\"}"),
        block_stop(1),
        msg_delta("tool_use"),
        stop(),
    ]);
    let second = sse(&[text_delta(0, "done"), msg_delta("end_turn"), stop()]);
    mount_two_rounds(&server, first, second).await;
    let mut turn = provider(&server).start(req()).await.unwrap();
    assert_eq!(turn.next().await, Some(ChatEvent::TextDelta("ok".into())));
    assert_eq!(
        turn.next().await,
        Some(ChatEvent::CommandProposed {
            call_id: "toolu_1".into(),
            command: "dir".into(),
            cwd: None
        })
    );
    turn.submit_command_result("toolu_1", "OUTPUT-X".into())
        .await
        .unwrap();
    assert_eq!(turn.next().await, Some(ChatEvent::TextDelta("done".into())));
    assert_eq!(turn.next().await, Some(ChatEvent::Done));
    assert_eq!(turn.next().await, None);
    let reqs = server.received_requests().await.unwrap();
    assert_eq!(reqs.len(), 2);
    let b: Value = reqs[1].body_json().unwrap();
    let m = b["messages"].as_array().unwrap();
    let n = m.len();
    assert_eq!(m[n - 2]["role"], "assistant");
    assert_eq!(m[n - 2]["content"][0], json!({"type":"text","text":"ok"}));
    assert_eq!(
        m[n - 2]["content"][1],
        json!({"type":"tool_use","id":"toolu_1","name":"run_command","input":{"command":"dir"}})
    );
    assert_eq!(m[n - 1]["role"], "user");
    assert_eq!(
        m[n - 1]["content"][0],
        json!({"type":"tool_result","tool_use_id":"toolu_1","content":"OUTPUT-X"})
    );
}

#[tokio::test]
async fn whitespace_only_text_block_is_not_echoed() {
    let server = MockServer::start().await;
    let first = sse(&[
        text_delta(0, "\n\n  "),
        block_stop(0),
        tool_start(1, "toolu_1", "run_command"),
        json_delta(1, "{\"command\":\"dir\"}"),
        block_stop(1),
        msg_delta("tool_use"),
        stop(),
    ]);
    let second = sse(&[text_delta(0, "done"), msg_delta("end_turn"), stop()]);
    mount_two_rounds(&server, first, second).await;
    let mut turn = provider(&server).start(req()).await.unwrap();
    loop {
        match turn.next().await {
            Some(ChatEvent::CommandProposed { .. }) => break,
            Some(ChatEvent::TextDelta(_)) => {}
            e => panic!("{e:?}"),
        }
    }
    turn.submit_command_result("toolu_1", "OUT".into())
        .await
        .unwrap();
    drain(&mut turn).await;
    let reqs = server.received_requests().await.unwrap();
    assert_eq!(reqs.len(), 2);
    let b: Value = reqs[1].body_json().unwrap();
    let m = b["messages"].as_array().unwrap();
    let assistant = &m[m.len() - 2];
    assert_eq!(assistant["role"], "assistant");
    let content = assistant["content"].as_array().unwrap();
    assert_eq!(content.len(), 1, "{content:?}");
    assert_eq!(content[0]["type"], "tool_use");
}

#[tokio::test]
async fn invalid_tool_input_yields_error_result_without_proposal() {
    let server = MockServer::start().await;
    let first = sse(&[
        tool_start(0, "toolu_bad", "run_command"),
        json_delta(0, "{\"command\": "),
        block_stop(0),
        tool_start(1, "toolu_unk", "other"),
        json_delta(1, "{}"),
        block_stop(1),
        msg_delta("tool_use"),
        stop(),
    ]);
    let second = sse(&[text_delta(0, "k"), msg_delta("end_turn"), stop()]);
    mount_two_rounds(&server, first, second).await;
    let mut turn = provider(&server).start(req()).await.unwrap();
    let evs = drain(&mut turn).await;
    assert_eq!(evs, vec![ChatEvent::TextDelta("k".into()), ChatEvent::Done]);
    let reqs = server.received_requests().await.unwrap();
    let b: Value = reqs[1].body_json().unwrap();
    let m = b["messages"].as_array().unwrap();
    let last = &m[m.len() - 1]["content"];
    assert_eq!(
        last[0],
        json!({"type":"tool_result","tool_use_id":"toolu_bad","content":"invalid run_command input","is_error":true})
    );
    assert_eq!(
        last[1],
        json!({"type":"tool_result","tool_use_id":"toolu_unk","content":"unknown tool","is_error":true})
    );
}

#[tokio::test]
async fn http_401_yields_error_event() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(401).set_body_json(
            json!({"error":{"type":"authentication_error","message":"invalid x-api-key"}}),
        ))
        .mount(&server)
        .await;
    let mut turn = provider(&server).start(req()).await.unwrap();
    match turn.next().await {
        Some(ChatEvent::Error(m)) => {
            assert!(m.contains("401"), "{m}");
            assert!(m.contains("invalid x-api-key"), "{m}");
            assert!(!m.contains("sk-secret"));
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(turn.next().await, None);
}

#[tokio::test]
async fn sse_error_event_becomes_error() {
    let server = MockServer::start().await;
    let body = sse(&[(
        "error",
        json!({"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}),
    )]);
    Mock::given(method("POST"))
        .respond_with(event_stream(body))
        .mount(&server)
        .await;
    let mut turn = provider(&server).start(req()).await.unwrap();
    match turn.next().await {
        Some(ChatEvent::Error(m)) => assert!(m.contains("Overloaded")),
        other => panic!("{other:?}"),
    }
    assert_eq!(turn.next().await, None);
}

#[tokio::test]
async fn cancel_ends_stream() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(event_stream(sse(&[
            text_delta(0, "a"),
            msg_delta("end_turn"),
            stop(),
        ])))
        .mount(&server)
        .await;
    let mut turn = provider(&server).start(req()).await.unwrap();
    turn.cancel().await;
    assert_eq!(turn.next().await, None);
}

#[tokio::test]
async fn no_key_is_unavailable() {
    let p = AnthropicProvider::new(
        Url::parse("https://api.anthropic.com").unwrap(),
        None,
        "m".into(),
    );
    assert_eq!(
        p.availability().await,
        Availability::Unavailable("ยังไม่ได้ตั้ง API key".into())
    );
    let p = AnthropicProvider::new(
        Url::parse("https://api.anthropic.com").unwrap(),
        Some("k".into()),
        "m".into(),
    );
    assert_eq!(p.availability().await, Availability::Ready);
}

#[test]
fn sse_parser_handles_one_byte_chunks_with_thai() {
    let text = "event: content_block_delta\ndata: {\"t\":\"สวัสดี\"}\n\nevent: ping\ndata: {}\n\n";
    let mut parser = SseParser::default();
    let mut out = vec![];
    for b in text.as_bytes() {
        out.extend(parser.push(std::slice::from_ref(b)));
    }
    assert_eq!(out.len(), 2);
    assert_eq!(out[0].event.as_deref(), Some("content_block_delta"));
    assert_eq!(out[0].data, "{\"t\":\"สวัสดี\"}");
    assert_eq!(out[1].event.as_deref(), Some("ping"));
}

fn tool_round(ids: &[&str]) -> String {
    let mut ev = vec![];
    for (i, id) in ids.iter().enumerate() {
        ev.push(tool_start(i, id, "run_command"));
        ev.push(json_delta(i, "{\"command\":\"dir\"}"));
        ev.push(block_stop(i));
    }
    ev.push(msg_delta("tool_use"));
    ev.push(stop());
    sse(&ev)
}

async fn drain_proposals(turn: &mut Box<dyn ProviderTurn>, n: usize) {
    for _ in 0..n {
        assert!(matches!(
            turn.next().await,
            Some(ChatEvent::CommandProposed { .. })
        ));
    }
}

#[tokio::test]
async fn no_submission_yields_error_then_none() {
    let server = MockServer::start().await;
    mount_two_rounds(
        &server,
        tool_round(&["a"]),
        sse(&[msg_delta("end_turn"), stop()]),
    )
    .await;
    let mut turn = provider(&server).start(req()).await.unwrap();
    drain_proposals(&mut turn, 1).await;
    match turn.next().await {
        Some(ChatEvent::Error(m)) => assert!(m.contains("tool results missing for: a"), "{m}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(turn.next().await, None);
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}

#[tokio::test]
async fn partial_submission_names_missing_id() {
    let server = MockServer::start().await;
    mount_two_rounds(
        &server,
        tool_round(&["a", "b"]),
        sse(&[msg_delta("end_turn"), stop()]),
    )
    .await;
    let mut turn = provider(&server).start(req()).await.unwrap();
    drain_proposals(&mut turn, 2).await;
    turn.submit_command_result("a", "x".into()).await.unwrap();
    match turn.next().await {
        Some(ChatEvent::Error(m)) => {
            assert!(m.contains("b"), "{m}");
            assert!(!m.contains("for: a"), "{m}");
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(turn.next().await, None);
}

#[tokio::test]
async fn tool_use_without_blocks_is_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(event_stream(sse(&[msg_delta("tool_use"), stop()])))
        .mount(&server)
        .await;
    let mut turn = provider(&server).start(req()).await.unwrap();
    assert_eq!(
        turn.next().await,
        Some(ChatEvent::Error("tool_use without tool calls".into()))
    );
    assert_eq!(turn.next().await, None);
}

#[tokio::test]
async fn dropping_next_mid_stream_then_cancel_ends() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            event_stream(sse(&[text_delta(0, "a"), msg_delta("end_turn"), stop()]))
                .set_delay(std::time::Duration::from_millis(500)),
        )
        .mount(&server)
        .await;
    let mut turn = provider(&server).start(req()).await.unwrap();
    let r = tokio::time::timeout(std::time::Duration::from_millis(50), turn.next()).await;
    assert!(r.is_err());
    turn.cancel().await;
    assert_eq!(turn.next().await, None);
}

#[tokio::test]
async fn dropping_next_during_second_request_does_not_duplicate_tool_result() {
    let server = MockServer::start().await;
    let second = event_stream(sse(&[text_delta(0, "done"), msg_delta("end_turn"), stop()]))
        .set_delay(std::time::Duration::from_millis(400));
    Mock::given(method("POST"))
        .and(body_string_contains("tool_result"))
        .respond_with(second)
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(event_stream(tool_round(&["a"])))
        .with_priority(5)
        .mount(&server)
        .await;
    let mut turn = provider(&server).start(req()).await.unwrap();
    drain_proposals(&mut turn, 1).await;
    turn.submit_command_result("a", "OUT".into()).await.unwrap();
    let r = tokio::time::timeout(std::time::Duration::from_millis(100), turn.next()).await;
    assert!(r.is_err());
    assert_eq!(turn.next().await, Some(ChatEvent::TextDelta("done".into())));
    assert_eq!(turn.next().await, Some(ChatEvent::Done));
    let reqs = server.received_requests().await.unwrap();
    assert_eq!(reqs.len(), 3);
    let b2: Value = reqs[1].body_json().unwrap();
    let b3: Value = reqs[2].body_json().unwrap();
    assert_eq!(b2["messages"], b3["messages"]);
    let m = b3["messages"].as_array().unwrap();
    let n = m
        .iter()
        .filter(|x| x["content"][0]["type"] == "tool_result")
        .count();
    assert_eq!(n, 1);
    assert_eq!(m.last().unwrap()["content"][0]["content"], "OUT");
}
