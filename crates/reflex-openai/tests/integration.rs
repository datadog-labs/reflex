// Unless explicitly stated otherwise all files in this repository are licensed under the Apache-2.0 License.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-present Datadog, Inc.

#[path = "../../openai-decisions/tests/support/mod.rs"]
mod support;
use openai_decisions::*;
use reflex::*;
use reflex_openai::*;
use serde::Serialize;
use serde_json::json;
use support::{Reply, Server};
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
enum Action {
    Open,
    Stay,
}
#[derive(Clone, PartialEq, Eq, Debug)]
enum Phase {
    Closed,
    Open,
}
#[tokio::test]
async fn provider_to_controller_to_executor_with_preserved_diagnostics() {
    let server = Server::start(vec![Reply::json(200, json!({"model":"gpt-6-luna","usage":{"input_tokens":8,"output_tokens":2},
        "answers":[{"type":"choice","name":"action","choice":"open","confidence":0.9,"probabilities":[{"value":"open","probability":0.95},{"value":"stay","probability":0.05}]}]}))]).await;
    let client = DecisionsClient::builder()
        .api_key("x")
        .endpoint(&server.endpoint)
        .build()
        .unwrap();
    let task = DecisionTask::builder()
        .model("gpt-6-luna")
        .questions(
            questions! { action: choice("Choose", [(Action::Open,"open"),(Action::Stay,"stay")]) },
        )
        .build()
        .unwrap();
    let controller = Controller::builder()
        .judge(DecisionsJudge::new(client, task).select_answer(|a| a.action))
        .build()
        .unwrap();
    let definition = state_machine! { phase: Phase, data: (), action: Action, event: (), no_change: Action::Stay,
        transitions: [Phase::Closed + action(Action::Open) => Phase::Open { min_confidence:0.85 }],
    };
    let executor = StateMachineExecutor::builder(definition)
        .store(InMemory::new(Phase::Closed, ()))
        .build()
        .unwrap();
    let outcome = executor
        .execute(controller.evaluate(&json!({"timeouts":30})).await)
        .await
        .unwrap();
    assert!(matches!(outcome, ExecutionOutcome::Applied(r) if r.to == Phase::Open));
    let diagnostic = controller.judge().last_response().unwrap();
    assert_eq!(diagnostic.model, "gpt-6-luna");
    assert_eq!(diagnostic.probabilities["open"], 0.95);
    assert_eq!(diagnostic.usage.unwrap().input_tokens, 8);
    assert_eq!(server.requests().len(), 1);
    server.finish().await;
}
#[tokio::test]
async fn provider_failure_reaches_error_transition() {
    let server = Server::start(vec![Reply::json(401, json!({}))]).await;
    let client = DecisionsClient::builder()
        .api_key("x")
        .endpoint(&server.endpoint)
        .build()
        .unwrap();
    let task = DecisionTask::builder()
        .model("gpt-6-luna")
        .questions(
            questions! { action: choice("Choose", [(Action::Open,"open"),(Action::Stay,"stay")]) },
        )
        .build()
        .unwrap();
    let controller = Controller::builder()
        .judge(DecisionsJudge::new(client, task).select_answer(|a| a.action))
        .build()
        .unwrap();
    let definition = state_machine! { phase: Phase, data: (), action: Action, event: (), transitions: [
        Phase::Closed + evaluation_error(EvaluationError::Judge(_)) => Phase::Open {},
    ]};
    let executor = StateMachineExecutor::builder(definition)
        .store(InMemory::new(Phase::Closed, ()))
        .build()
        .unwrap();
    let ExecutionOutcome::Applied(receipt) = executor
        .execute(controller.evaluate(&json!({})).await)
        .await
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(receipt.to, Phase::Open);
    assert!(
        matches!(receipt.evaluation_error, Some(EvaluationError::Judge(e)) if e.message.contains("401"))
    );
    server.finish().await;
}
#[tokio::test]
async fn refusal_reaches_the_executor_as_a_judge_error() {
    let server = Server::start(vec![Reply::json(
        200,
        json!({"model":"gpt-6-luna","answers":[{"type":"refusal","name":"action"}]}),
    )])
    .await;
    let client = DecisionsClient::builder()
        .api_key("x")
        .endpoint(&server.endpoint)
        .build()
        .unwrap();
    let task = DecisionTask::builder()
        .model("gpt-6-luna")
        .questions(
            questions! { action: choice("Choose", [(Action::Open,"open"),(Action::Stay,"stay")]) },
        )
        .build()
        .unwrap();
    let controller = Controller::builder()
        .judge(DecisionsJudge::new(client, task).select_answer(|a| a.action))
        .build()
        .unwrap();
    let evaluation = controller.evaluate(&json!({})).await;
    assert!(matches!(evaluation, Err(EvaluationError::Judge(e)) if e.code == "openai_refusal"));
    assert!(controller.judge().last_response().is_none());
    server.finish().await;
}
