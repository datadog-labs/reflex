// Unless explicitly stated otherwise all files in this repository are licensed under the Apache-2.0 License.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-present Datadog, Inc.

//! Live evaluation only. Run explicitly with TYPESAFE_API_KEY set.
use reflex::Controller;
use reflex_typesafe::TypeSafeJudge;
use serde::Serialize;
use std::time::Duration;
use typesafe_ai::{choice, questions, SystemOneTask, TypeSafeClient};
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
enum Action {
    Open,
    PermitProbe,
    NoChange,
}
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = TypeSafeClient::builder()
        .api_key(std::env::var("TYPESAFE_API_KEY")?)
        .timeout(Duration::from_secs(2))
        .build()?;
    let task = SystemOneTask::builder()
        .model("jev-1.13.0")
        .questions(questions! {
            action: choice("Recommend the next circuit action. Use only supplied evidence.", [
                (Action::Open, "Open under sustained distress"),
                (Action::PermitProbe, "Permit one probe when open and cooldown has elapsed"),
                (Action::NoChange, "Leave the circuit unchanged"),
            ]),
        })
        .build()?;
    let judge = TypeSafeJudge::new(client, task).select_answer(|answers| answers.action);
    let controller = Controller::builder()
        .judge(judge)
        .inference_timeout(Duration::from_secs(2))
        .build()?;
    let state = serde_json::json!({"phase":"closed","completed_requests":100,"timeouts":75,"can_probe":false});
    let evaluation = controller.evaluate(&state).await;
    println!("{evaluation:?}");
    println!(
        "Provider diagnostics: {:?}",
        controller.judge().last_response()
    );
    // Pass `evaluation` intact to your executor; the core circuit_breaker example
    // demonstrates guarded execution and automatic effect completion.
    Ok(())
}
