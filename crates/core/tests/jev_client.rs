use assistant_core::jev::*;
use indexmap::IndexMap;
use std::time::Duration;
use url::Url;
use wiremock::matchers::{body_partial_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn questions() -> IndexMap<String, JevQuestion> {
    let mut qs = IndexMap::new();
    let mut criteria = IndexMap::new();
    criteria.insert("api".to_string(), "use API".to_string());
    criteria.insert("hermes".to_string(), "use Hermes".to_string());
    qs.insert(
        "route".to_string(),
        JevQuestion::Choice {
            instructions: "pick".into(),
            criteria,
        },
    );
    qs.insert(
        "is_safe".to_string(),
        JevQuestion::Noul {
            instructions: "safe?".into(),
        },
    );
    qs
}

fn client(s: &MockServer) -> JevClient {
    JevClient::new(Url::parse(&s.uri()).unwrap(), "k".into())
}

#[tokio::test]
async fn sends_bearer_and_parses_choice_and_noul() {
    let s = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/systemone"))
        .and(header("Authorization", "Bearer k"))
        .and(body_partial_json(serde_json::json!({
            "state": "ภาษาไทย",
            "model": "jev-latest",
            "questions": {
                "route": {"type":"choice","instructions":"pick","criteria":{"api":"use API","hermes":"use Hermes"}},
                "is_safe": {"type":"noul","instructions":"safe?"}
            }
        })))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"{"model":"jev-1.13.0","answers":{"route":{"type":"choice","choice":"api","confidence":0.8,"probabilities":{"api":0.9,"hermes":0.1}},"is_safe":{"type":"noul","noul":0.97},"s":{"type":"score","score":3},"b":{"type":"box","x":1}},"usage":{"input_tokens":1,"output_tokens":1}}"#,
        ))
        .mount(&s)
        .await;
    let r = client(&s).ask("ภาษาไทย", questions()).await.unwrap();
    assert_eq!(r.model, "jev-1.13.0");
    assert_eq!(r.choice("route").unwrap().0, "api");
    assert_eq!(r.choice("route").unwrap().1["hermes"], 0.1);
    assert_eq!(r.noul("is_safe").unwrap(), 0.97);
    assert!(matches!(r.noul("route"), Err(JevError::MissingAnswer(_))));
    assert!(matches!(r.choice("nope"), Err(JevError::MissingAnswer(_))));
    assert!(matches!(r.noul("s"), Err(JevError::MissingAnswer(_))));
    assert!(matches!(r.answers["b"], JevAnswer::Other));
}

#[tokio::test]
async fn http_500_is_http_error() {
    let s = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(500).set_body_string("boom"))
        .mount(&s)
        .await;
    let err = client(&s).ask("x", questions()).await.unwrap_err();
    assert!(matches!(&err, JevError::Http(500, b) if b == "boom"));
}

#[tokio::test]
async fn garbage_body_is_decode_error() {
    let s = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_string("not json"))
        .mount(&s)
        .await;
    let err = client(&s).ask("x", questions()).await.unwrap_err();
    assert!(matches!(err, JevError::Decode(_)));
}

#[tokio::test]
async fn slow_server_is_transport_error() {
    let s = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_millis(600)))
        .mount(&s)
        .await;
    let c = JevClient::with_timeout(
        Url::parse(&s.uri()).unwrap(),
        "k".into(),
        Duration::from_millis(200),
    );
    let err = c.ask("x", questions()).await.unwrap_err();
    assert!(matches!(err, JevError::Transport(_)));
}
