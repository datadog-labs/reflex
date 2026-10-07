// Unless explicitly stated otherwise all files in this repository are licensed under the Apache-2.0 License.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-present Datadog, Inc.

#[path = "../../openai-decisions/tests/support/mod.rs"]
mod support;
use openai_decisions::DecisionsClient;
use reflex_sim::{
    jev::{Choice, Evaluator, Evidence, LiveEvaluator, Window},
    policy::CircuitPhase,
};
use serde_json::{json, Value};
use support::{Reply, Server};

fn evidence() -> Evidence {
    Evidence {
        forecast: None,
        service: "payments".into(),
        telemetry: None,
        control_since_unix_ms: 0,
        observed_at_ms: 5000.0,
        phase: CircuitPhase::Closed,
        revision: 3,
        last_5_seconds: Window {
            responses: 100,
            successes: 25,
            errors: 0,
            timeouts: 75,
            failure_ratio: 0.75,
            p95_latency_ms: Some(900.0),
        },
        last_1_second: Window::default(),
        cooldown_remaining_ms: 0.0,
        client_timeout_ms: 1000.0,
        legal_actions: vec![Choice::Open, Choice::NoChange],
    }
}
fn evaluator(server: &Server) -> LiveEvaluator {
    let client = DecisionsClient::builder()
        .api_key("x")
        .endpoint(&server.endpoint)
        .max_retries(0)
        .build()
        .unwrap();
    LiveEvaluator::new(client, "gpt-6-luna".into())
}
#[tokio::test]
async fn openai_decisions_answers_the_circuit_breaker_question() {
    let mut reply = Reply::json(
        200,
        json!({"model":"gpt-6-luna","usage":{"input_tokens":400,"output_tokens":0},
            "answers":[{"type":"choice","name":"action","choice":"open","confidence":0.7,
                "probabilities":[{"value":"open","probability":0.8},{"value":"probe","probability":0.05},
                    {"value":"no_change","probability":0.15}]}]}),
    );
    reply.headers = "X-Request-Id: req-1\r\n".into();
    let server = Server::start(vec![reply]).await;
    let input = evidence().model_input();
    let inference = evaluator(&server).evaluate(evidence()).await;
    assert!(inference.error.is_none());
    assert_eq!(inference.action, Some(Choice::Open));
    assert_eq!(inference.confidence, Some(0.7));
    assert_eq!(inference.probabilities["open"], 0.8);
    assert_eq!(inference.model.as_deref(), Some("gpt-6-luna"));
    assert_eq!(inference.usage.unwrap().input_tokens, 400);
    assert_eq!(inference.request_id.as_deref(), Some("req-1"));
    let requests = server.requests();
    let request = &requests[0].1;
    assert!(requests[0].0.starts_with("POST /v1/decisions HTTP/1.1"));
    assert_eq!(request["model"], "gpt-6-luna");
    // The same evidence Jev receives, as JSON text.
    let sent: Value = serde_json::from_str(request["input"].as_str().unwrap()).unwrap();
    assert_eq!(sent, input);
    let question = &request["questions"][0];
    assert_eq!(question["name"], "action");
    assert_eq!(question["type"], "choice");
    let values: Vec<_> = question["choices"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["value"].as_str().unwrap())
        .collect();
    assert_eq!(values, ["open", "probe", "no_change"]);
    server.finish().await;
}
#[tokio::test]
async fn an_openai_refusal_is_a_failed_inference_not_an_action() {
    let server = Server::start(vec![Reply::json(
        200,
        json!({"model":"gpt-6-luna","answers":[{"type":"refusal","name":"action"}]}),
    )])
    .await;
    let inference = evaluator(&server).evaluate(evidence()).await;
    assert_eq!(inference.action, None);
    assert_eq!(inference.error.unwrap().code, "openai_refusal");
    server.finish().await;
}
// The Jev path keeps its wire contract when built through the shared judge.
#[tokio::test]
async fn typesafe_still_receives_structured_state_and_keyed_questions() {
    let server = Server::start(vec![Reply::json(
        200,
        json!({"model":"jev-1.13.0","usage":{"input_tokens":300,"output_tokens":2},
            "answers":{"action":{"type":"choice","choice":"no_change","confidence":0.6,
                "probabilities":{"open":0.2,"probe":0.1,"no_change":0.7}}}}),
    )])
    .await;
    let client = typesafe_ai::TypeSafeClient::builder()
        .api_key("x")
        .endpoint(&server.endpoint)
        .max_retries(0)
        .build()
        .unwrap();
    let input = evidence().model_input();
    let inference = LiveEvaluator::new(client, "jev-1.13.0".into())
        .evaluate(evidence())
        .await;
    assert!(inference.error.is_none());
    assert_eq!(inference.action, Some(Choice::NoChange));
    assert_eq!(inference.model.as_deref(), Some("jev-1.13.0"));
    assert_eq!(inference.usage.unwrap().output_tokens, 2);
    let requests = server.requests();
    let request = &requests[0].1;
    assert_eq!(request["model"], "jev-1.13.0");
    assert_eq!(request["state"], input);
    let question = &request["questions"]["action"];
    assert_eq!(question["type"], "choice");
    let mut labels: Vec<_> = question["criteria"].as_object().unwrap().keys().collect();
    labels.sort();
    assert_eq!(labels, ["no_change", "open", "probe"]);
    server.finish().await;
}
