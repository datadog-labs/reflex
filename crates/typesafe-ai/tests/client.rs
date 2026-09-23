// Unless explicitly stated otherwise all files in this repository are licensed under the Apache-2.0 License.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-present Datadog, Inc.

mod support;
use serde::Serialize;
use serde_json::json;
use std::time::Duration;
use support::{Reply, Server};
use typesafe_ai::*;
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Action {
    Admit,
    Defer,
}
fn response() -> serde_json::Value {
    json!({
        "model":"jev-1.13.0", "usage":{"input_tokens":12,"output_tokens":3,"custom":7},
        "answers":{
            "action":{"type":"choice","choice":"admit","confidence":0.8,"probabilities":{"admit":0.9,"defer":0.1}},
            "urgency":{"type":"noul","noul":0.9},
            "load":{"type":"score","score":0.75,"legend":{"0":"low","1":"high"},"probabilities":{"0":0.25,"1":0.75},"confidence":0.6}
        }
    })
}
macro_rules! task {
    () => {
        SystemOneTask::builder()
            .model("jev-1.13.0")
            .questions(questions! {
                action: choice("Admit?", [(Action::Admit,"accept"),(Action::Defer,"wait")]),
                urgency: noul("Urgent?"), load: score("Load?", ["low","high"]),
            })
            .build()
            .unwrap()
    };
}
#[tokio::test]
async fn typed_round_trip_preserves_metadata_and_sends_exact_wire_contract() {
    let mut reply = Reply::json(200, response());
    reply.headers = "X-Request-Id: req-123\r\n".into();
    let server = Server::start(vec![reply]).await;
    let client = TypeSafeClient::builder()
        .api_key("test-key")
        .endpoint(&server.endpoint)
        .build()
        .unwrap();
    let result = client
        .system_one(&task!(), &json!({"load":42}))
        .await
        .unwrap();
    assert_eq!(result.answers.action.choice, Action::Admit);
    assert_eq!(result.answers.action.confidence, Some(0.8));
    assert_eq!(result.answers.action.probabilities["admit"], 0.9);
    assert_eq!(result.answers.urgency.noul, 0.9);
    assert_eq!(result.answers.load.score, 0.75);
    assert_eq!(result.usage.extra["custom"], 7);
    assert_eq!(result.request_id.as_deref(), Some("req-123"));
    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert!(requests[0].0.starts_with("POST /v1/systemone HTTP/1.1"));
    assert!(requests[0]
        .0
        .to_lowercase()
        .contains("authorization: bearer test-key"));
    assert_eq!(requests[0].1["state"], json!({"load":42}));
    assert_eq!(
        requests[0].1["questions"]["action"]["criteria"],
        json!({"admit":"accept","defer":"wait"})
    );
    assert_eq!(
        requests[0].1["questions"]["load"]["criteria"],
        json!(["low", "high"])
    );
    server.finish().await;
}
#[tokio::test]
async fn retries_retryable_status_with_same_request() {
    let mut retry = Reply::json(429, json!({}));
    retry.headers = "Retry-After: 0\r\n".into();
    let server = Server::start(vec![retry, Reply::json(200, response())]).await;
    let client = TypeSafeClient::builder()
        .api_key("x")
        .endpoint(&server.endpoint)
        .build()
        .unwrap();
    client.system_one(&task!(), &json!({})).await.unwrap();
    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].1, requests[1].1);
    server.finish().await;
}
#[tokio::test]
async fn authentication_errors_are_not_retried() {
    let server = Server::start(vec![Reply::json(401, json!({}))]).await;
    let client = TypeSafeClient::builder()
        .api_key("x")
        .endpoint(&server.endpoint)
        .build()
        .unwrap();
    assert!(matches!(
        client.system_one(&task!(), &json!({})).await,
        Err(Error::Http { status: 401, .. })
    ));
    assert_eq!(server.requests().len(), 1);
    server.finish().await;
}
#[tokio::test]
async fn overall_deadline_includes_backoff() {
    let mut retry = Reply::json(529, json!({}));
    retry.headers = "Retry-After: 60\r\n".into();
    let server = Server::start(vec![retry]).await;
    let client = TypeSafeClient::builder()
        .api_key("x")
        .endpoint(&server.endpoint)
        .timeout(Duration::from_millis(50))
        .build()
        .unwrap();
    assert!(matches!(
        client.system_one(&task!(), &json!({})).await,
        Err(Error::Timeout)
    ));
    assert_eq!(server.requests().len(), 1);
    server.finish().await;
}
#[tokio::test]
async fn redirect_does_not_forward_credentials() {
    let mut reply = Reply::json(302, json!({}));
    reply.headers = "Location: https://example.com/\r\n".into();
    let server = Server::start(vec![reply]).await;
    let client = TypeSafeClient::builder()
        .api_key("x")
        .endpoint(&server.endpoint)
        .build()
        .unwrap();
    assert!(matches!(
        client.system_one(&task!(), &json!({})).await,
        Err(Error::Http { status: 302, .. })
    ));
    server.finish().await;
}
#[tokio::test]
async fn rejects_unknown_choice_wrong_types_distributions_and_answer_names() {
    let mut bad = Vec::new();
    let mut v = response();
    v["answers"]["action"]["choice"] = json!("unknown");
    bad.push(v);
    let mut v = response();
    v["answers"]["action"]["probabilities"]["admit"] = json!(1.2);
    bad.push(v);
    let mut v = response();
    v["answers"]["action"]["confidence"] = json!(1.2);
    bad.push(v);
    let mut v = response();
    v["answers"]["action"]["type"] = json!("score");
    bad.push(v);
    let mut v = response();
    v["answers"]["action"]["choice"] = json!("defer");
    bad.push(v);
    let mut v = response();
    v["answers"]["extra"] = json!({});
    bad.push(v);
    let mut v = response();
    v["answers"].as_object_mut().unwrap().remove("urgency");
    bad.push(v);
    let mut v = response();
    v["answers"]["load"]["score"] = json!(3);
    bad.push(v);
    let mut v = response();
    v["answers"]["urgency"]["noul"] = json!(-0.2);
    bad.push(v);
    let mut v = response();
    v["answers"]["load"]["legend"]["0"] = json!("wrong");
    bad.push(v);
    for body in bad {
        let server = Server::start(vec![Reply::json(200, body)]).await;
        let client = TypeSafeClient::builder()
            .api_key("x")
            .endpoint(&server.endpoint)
            .build()
            .unwrap();
        assert!(matches!(
            client.system_one(&task!(), &json!({})).await,
            Err(Error::InvalidResponse(_))
        ));
        server.finish().await;
    }
}
#[tokio::test]
async fn missing_confidence_is_not_fabricated() {
    let mut body = response();
    body["answers"]["action"]
        .as_object_mut()
        .unwrap()
        .remove("confidence");
    let server = Server::start(vec![Reply::json(200, body)]).await;
    let client = TypeSafeClient::builder()
        .api_key("x")
        .endpoint(&server.endpoint)
        .build()
        .unwrap();
    assert_eq!(
        client
            .system_one(&task!(), &json!({}))
            .await
            .unwrap()
            .answers
            .action
            .confidence,
        None
    );
    server.finish().await;
}
#[test]
fn configuration_rejects_ambiguous_labels_and_invalid_credentials() {
    assert!(choice("Pick", [("x", "one"), ("x", "two")])
        .encode()
        .is_err());
    assert!(score("Rate", ["only"]).encode().is_err());
    assert!(TypeSafeClient::builder().api_key("\r\n").build().is_err());
    assert!(TypeSafeClient::builder()
        .api_key("x")
        .endpoint("http://example.com")
        .build()
        .is_err());
    assert!(TypeSafeClient::builder()
        .api_key("x")
        .endpoint("https://user:pass@example.com")
        .build()
        .is_err());
    assert!(TypeSafeClient::builder()
        .api_key("x")
        .max_retries(11)
        .build()
        .is_err());
}
