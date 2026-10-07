// Unless explicitly stated otherwise all files in this repository are licensed under the Apache-2.0 License.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-present Datadog, Inc.

mod support;
use openai_decisions::*;
use serde::Serialize;
use serde_json::json;
use std::time::Duration;
use support::{Reply, Server};
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Action {
    Admit,
    Defer,
}
fn response() -> serde_json::Value {
    json!({
        "model":"gpt-6-luna", "usage":{"input_tokens":12,"output_tokens":0,"total_tokens":12},
        "answers":[
            {"type":"choice","name":"action","choice":"admit","confidence":0.8,
                "probabilities":[{"value":"admit","probability":0.9},{"value":"defer","probability":0.1}]},
            {"type":"predicate","name":"urgency","probability":0.9},
            {"type":"score","name":"load","score":0.75,"confidence":0.6,
                "probabilities":[{"value":0,"label":"low","probability":0.25},{"value":1,"label":"high","probability":0.75}]}
        ]
    })
}
macro_rules! task {
    () => {
        DecisionTask::builder()
            .model("gpt-6-luna")
            .questions(questions! {
                action: choice("Admit?", [(Action::Admit,"accept"),(Action::Defer,"wait")]),
                urgency: predicate("Urgent?"),
                load: score("Load?", [("low","Little work"),("high","Much work")]),
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
    let client = DecisionsClient::builder()
        .api_key("test-key")
        .endpoint(&server.endpoint)
        .build()
        .unwrap();
    let result = client.decide(&task!(), &json!({"load":42})).await.unwrap();
    assert_eq!(result.answers.action.choice, Action::Admit);
    assert_eq!(result.answers.action.confidence, Some(0.8));
    assert_eq!(result.answers.action.probabilities["admit"], 0.9);
    assert_eq!(result.answers.urgency.probability, 0.9);
    assert_eq!(result.answers.load.score, 0.75);
    assert_eq!(result.answers.load.confidence, Some(0.6));
    assert_eq!(
        result.answers.load.probabilities,
        [
            LevelProbability {
                label: "low".into(),
                probability: 0.25
            },
            LevelProbability {
                label: "high".into(),
                probability: 0.75
            },
        ]
    );
    assert_eq!(result.model, "gpt-6-luna");
    let usage = result.usage.unwrap();
    assert_eq!((usage.input_tokens, usage.output_tokens), (12, 0));
    assert_eq!(usage.extra["total_tokens"], 12);
    assert_eq!(result.request_id.as_deref(), Some("req-123"));
    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert!(requests[0].0.starts_with("POST /v1/decisions HTTP/1.1"));
    assert!(requests[0]
        .0
        .to_lowercase()
        .contains("authorization: bearer test-key"));
    assert_eq!(
        requests[0].1,
        json!({
            "model":"gpt-6-luna",
            "input":"{\"load\":42}",
            "questions":[
                {"type":"choice","name":"action","instructions":"Admit?","choices":[
                    {"value":"admit","description":"accept"},{"value":"defer","description":"wait"}]},
                {"type":"predicate","name":"urgency","instructions":"Urgent?"},
                {"type":"score","name":"load","instructions":"Load?","levels":[
                    {"label":"low","description":"Little work"},{"label":"high","description":"Much work"}]}
            ]
        })
    );
    server.finish().await;
}
#[tokio::test]
async fn a_named_client_labels_its_metrics_and_an_unnamed_one_does_not() {
    use opentelemetry::metrics::MeterProvider;
    use opentelemetry_sdk::metrics::{
        data::{AggregatedMetrics, MetricData},
        InMemoryMetricExporter, PeriodicReader, SdkMeterProvider,
    };
    for name in [Some("cluster_autoscaler"), None] {
        let exporter = InMemoryMetricExporter::default();
        let provider = SdkMeterProvider::builder()
            .with_reader(PeriodicReader::builder(exporter.clone()).build())
            .build();
        let server = Server::start(vec![Reply::json(200, response())]).await;
        let mut builder = DecisionsClient::builder()
            .api_key("test-key")
            .endpoint(&server.endpoint)
            .meter(provider.meter("openai-decisions"));
        if let Some(name) = name {
            builder = builder.name(name);
        }
        let client = builder.build().unwrap();
        client.decide(&task!(), &json!({"load":42})).await.unwrap();
        provider.force_flush().unwrap();
        let metrics = exporter.get_finished_metrics().unwrap();
        let mut labels = Vec::new();
        for metric in metrics
            .iter()
            .flat_map(|rm| rm.scope_metrics())
            .flat_map(|sm| sm.metrics())
        {
            let client: Vec<Vec<String>> = match metric.data() {
                AggregatedMetrics::U64(MetricData::Sum(sum)) => sum
                    .data_points()
                    .map(|p| p.attributes().map(|kv| kv.to_owned()).collect::<Vec<_>>())
                    .map(|a| {
                        a.iter()
                            .filter(|kv| kv.key.as_str() == "client")
                            .map(|kv| kv.value.to_string())
                            .collect()
                    })
                    .collect(),
                AggregatedMetrics::F64(MetricData::Histogram(h)) => h
                    .data_points()
                    .map(|p| p.attributes().map(|kv| kv.to_owned()).collect::<Vec<_>>())
                    .map(|a| {
                        a.iter()
                            .filter(|kv| kv.key.as_str() == "client")
                            .map(|kv| kv.value.to_string())
                            .collect()
                    })
                    .collect(),
                // The in-flight gauge is process-wide and never labelled.
                _ => continue,
            };
            labels.push((metric.name().to_owned(), client));
        }
        for expected in [
            "openai.decisions.client.requests",
            "openai.decisions.client.request.duration",
            "openai.decisions.client.call.duration",
            "openai.decisions.client.tokens",
        ] {
            let (_, points) = labels.iter().find(|(n, _)| n == expected).unwrap();
            assert!(!points.is_empty(), "{expected}");
            for point in points {
                let wanted: Vec<String> = name.iter().map(|n| n.to_string()).collect();
                assert_eq!(point, &wanted, "{expected}");
            }
        }
        provider.shutdown().unwrap();
    }
}
#[tokio::test]
async fn retries_retryable_status_with_same_request() {
    let mut retry = Reply::json(429, json!({}));
    retry.headers = "Retry-After: 0\r\n".into();
    let server = Server::start(vec![retry, Reply::json(200, response())]).await;
    let client = DecisionsClient::builder()
        .api_key("x")
        .endpoint(&server.endpoint)
        .build()
        .unwrap();
    client.decide(&task!(), &json!({})).await.unwrap();
    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].1, requests[1].1);
    server.finish().await;
}
#[tokio::test]
async fn authentication_errors_are_not_retried() {
    let server = Server::start(vec![Reply::json(401, json!({}))]).await;
    let client = DecisionsClient::builder()
        .api_key("x")
        .endpoint(&server.endpoint)
        .build()
        .unwrap();
    assert!(matches!(
        client.decide(&task!(), &json!({})).await,
        Err(Error::Http { status: 401, .. })
    ));
    assert_eq!(server.requests().len(), 1);
    server.finish().await;
}
#[tokio::test]
async fn overall_deadline_includes_backoff() {
    let mut retry = Reply::json(503, json!({}));
    retry.headers = "Retry-After: 60\r\n".into();
    let server = Server::start(vec![retry]).await;
    let client = DecisionsClient::builder()
        .api_key("x")
        .endpoint(&server.endpoint)
        .timeout(Duration::from_millis(50))
        .build()
        .unwrap();
    assert!(matches!(
        client.decide(&task!(), &json!({})).await,
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
    let client = DecisionsClient::builder()
        .api_key("x")
        .endpoint(&server.endpoint)
        .build()
        .unwrap();
    assert!(matches!(
        client.decide(&task!(), &json!({})).await,
        Err(Error::Http { status: 302, .. })
    ));
    server.finish().await;
}
#[tokio::test]
async fn rejects_unknown_choice_wrong_types_distributions_and_answer_names() {
    let mut bad = Vec::new();
    let mut v = response();
    v["answers"][0]["choice"] = json!("unknown");
    bad.push(v);
    let mut v = response();
    v["answers"][0]["probabilities"][0]["probability"] = json!(1.2);
    bad.push(v);
    let mut v = response();
    v["answers"][0]["probabilities"][1]["value"] = json!("admit");
    bad.push(v);
    let mut v = response();
    v["answers"][0]["probabilities"] = json!({"admit":0.9,"defer":0.1});
    bad.push(v);
    let mut v = response();
    v["answers"][0]["confidence"] = json!(1.2);
    bad.push(v);
    let mut v = response();
    v["answers"][0]["type"] = json!("score");
    bad.push(v);
    let mut v = response();
    v["answers"][0]["choice"] = json!("defer");
    bad.push(v);
    let mut v = response();
    v["answers"]
        .as_array_mut()
        .unwrap()
        .push(json!({"type":"predicate","name":"extra","probability":0.5}));
    bad.push(v);
    let mut v = response();
    v["answers"].as_array_mut().unwrap().remove(1);
    bad.push(v);
    let mut v = response();
    v["answers"][1]["name"] = json!("action");
    bad.push(v);
    let mut v = response();
    v["answers"] = json!({"action":{}, "urgency":{}, "load":{}});
    bad.push(v);
    let mut v = response();
    v["answers"][2]["score"] = json!(3);
    bad.push(v);
    let mut v = response();
    v["answers"][1]["probability"] = json!(-0.2);
    bad.push(v);
    let mut v = response();
    v["answers"][2]["probabilities"][0]["label"] = json!("wrong");
    bad.push(v);
    let mut v = response();
    v["answers"][2]["probabilities"][1]["value"] = json!(0);
    bad.push(v);
    let mut v = response();
    v["model"] = json!(7);
    bad.push(v);
    let mut v = response();
    v["usage"] = json!({"input_tokens":"many"});
    bad.push(v);
    for body in bad {
        let server = Server::start(vec![Reply::json(200, body)]).await;
        let client = DecisionsClient::builder()
            .api_key("x")
            .endpoint(&server.endpoint)
            .build()
            .unwrap();
        assert!(matches!(
            client.decide(&task!(), &json!({})).await,
            Err(Error::InvalidResponse(_))
        ));
        server.finish().await;
    }
}
#[tokio::test]
async fn missing_confidence_is_not_fabricated() {
    let mut body = response();
    body["answers"][0]
        .as_object_mut()
        .unwrap()
        .remove("confidence");
    let server = Server::start(vec![Reply::json(200, body)]).await;
    let client = DecisionsClient::builder()
        .api_key("x")
        .endpoint(&server.endpoint)
        .build()
        .unwrap();
    assert_eq!(
        client
            .decide(&task!(), &json!({}))
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
    assert!(score("Rate", [("only", "one")]).encode().is_err());
    assert!(score("Rate", [("same", "one"), ("same", "two")])
        .encode()
        .is_err());
    assert!(predicate(" ").encode().is_err());
    assert!(DecisionsClient::builder().api_key("\r\n").build().is_err());
    assert!(DecisionsClient::builder()
        .api_key("x")
        .endpoint("http://example.com")
        .build()
        .is_err());
    assert!(DecisionsClient::builder()
        .api_key("x")
        .endpoint("https://user:pass@example.com")
        .build()
        .is_err());
    assert!(DecisionsClient::builder()
        .api_key("x")
        .max_retries(11)
        .build()
        .is_err());
}
#[tokio::test]
async fn a_refused_question_fails_the_call_and_names_the_question() {
    let mut body = response();
    body["answers"][1] = json!({"type":"refusal","name":"urgency"});
    let server = Server::start(vec![Reply::json(200, body)]).await;
    let client = DecisionsClient::builder()
        .api_key("x")
        .endpoint(&server.endpoint)
        .build()
        .unwrap();
    let error = client.decide(&task!(), &json!({})).await.unwrap_err();
    assert!(matches!(&error, Error::Refusal { question } if question == "urgency"));
    assert_eq!(error.code(), "refusal");
    assert_eq!(server.requests().len(), 1);
    server.finish().await;
}
#[tokio::test]
async fn missing_model_and_usage_are_not_fabricated() {
    let mut body = response();
    body.as_object_mut().unwrap().remove("model");
    body.as_object_mut().unwrap().remove("usage");
    let server = Server::start(vec![Reply::json(200, body)]).await;
    let client = DecisionsClient::builder()
        .api_key("x")
        .endpoint(&server.endpoint)
        .build()
        .unwrap();
    let result = client.decide(&task!(), "plain text").await.unwrap();
    assert_eq!(result.model, "gpt-6-luna");
    assert!(result.usage.is_none());
    assert_eq!(server.requests()[0].1["input"], "plain text");
    server.finish().await;
}
#[tokio::test]
async fn http_errors_keep_the_provider_code_but_not_its_message() {
    let server = Server::start(vec![Reply::json(
        404,
        json!({"error":{"message":"secret-message","type":"invalid_request_error","param":null,"code":"model_not_found"}}),
    )])
    .await;
    let client = DecisionsClient::builder()
        .api_key("x")
        .endpoint(&server.endpoint)
        .build()
        .unwrap();
    let error = client.decide(&task!(), &json!({})).await.unwrap_err();
    assert!(
        matches!(&error, Error::Http { status: 404, code: Some(code), .. } if code == "model_not_found")
    );
    assert_eq!(error.to_string(), "OpenAI HTTP 404 (model_not_found)");
    assert!(!format!("{error:?}").contains("secret-message"));
    server.finish().await;
}
#[test]
fn input_must_be_text_or_structured() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let client = runtime.block_on(async {
        DecisionsClient::builder()
            .api_key("x")
            .endpoint("http://127.0.0.1:9/v1/decisions")
            .build()
            .unwrap()
    });
    assert!(matches!(
        runtime.block_on(client.decide(&task!(), &42)),
        Err(Error::Configuration(_))
    ));
}
// The request and response bodies published in the Decisions guide.
#[test]
fn the_documented_examples_encode_and_decode() {
    let damage = predicate("Does the product have visible damage, such as a crack, tear, or dent? Ignore shadows and damage to the packaging.");
    let department = choice(
        "Which department should handle this complaint?",
        [
            ("billing", "Payments, invoices, and refunds."),
            ("technical", "Problems using the product."),
            ("shipping", "Delivery and tracking."),
            ("other", "Requests outside these categories."),
        ],
    );
    let severity = score(
        "How severe is this issue?",
        [
            ("Cosmetic", "Appearance only; no lost functionality."),
            (
                "Workaround available",
                "A task fails, but another way works.",
            ),
            ("Fully blocked", "A task fails with no workaround."),
        ],
    );
    let set = questions! { visible_damage: damage, department: department, severity: severity };
    assert_eq!(
        set.encode().unwrap(),
        json!([
            {"type":"predicate","name":"visible_damage","instructions":"Does the product have visible damage, such as a crack, tear, or dent? Ignore shadows and damage to the packaging."},
            {"type":"choice","name":"department","instructions":"Which department should handle this complaint?","choices":[
                {"value":"billing","description":"Payments, invoices, and refunds."},
                {"value":"technical","description":"Problems using the product."},
                {"value":"shipping","description":"Delivery and tracking."},
                {"value":"other","description":"Requests outside these categories."}]},
            {"type":"score","name":"severity","instructions":"How severe is this issue?","levels":[
                {"label":"Cosmetic","description":"Appearance only; no lost functionality."},
                {"label":"Workaround available","description":"A task fails, but another way works."},
                {"label":"Fully blocked","description":"A task fails with no workaround."}]}
        ])
    );
    let answers = set
        .decode(&json!([
            {"type":"predicate","name":"visible_damage","probability":0.92},
            {"type":"choice","name":"department","choice":"billing","probabilities":[
                {"value":"billing","probability":0.95},{"value":"technical","probability":0.02},
                {"value":"shipping","probability":0.01},{"value":"other","probability":0.02}],
                "confidence":0.93},
            {"type":"score","name":"severity","score":1.1,"probabilities":[
                {"value":0,"label":"Cosmetic","probability":0.1},
                {"value":1,"label":"Workaround available","probability":0.7},
                {"value":2,"label":"Fully blocked","probability":0.2}],
                "confidence":0.55}
        ]))
        .unwrap();
    assert_eq!(answers.visible_damage.probability, 0.92);
    assert_eq!(answers.department.choice, "billing");
    assert_eq!(answers.department.confidence, Some(0.93));
    assert_eq!(answers.department.probabilities["other"], 0.02);
    assert_eq!(answers.severity.score, 1.1);
    assert_eq!(
        answers.severity.probabilities[1].label,
        "Workaround available"
    );
    assert_eq!(answers.severity.probabilities[1].probability, 0.7);
}
