// Unless explicitly stated otherwise all files in this repository are licensed under the Apache-2.0 License.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-present Datadog, Inc.

//! One live TypeSafe call, exporting metrics, traces and logs directly to Datadog.
//! See ../TELEMETRY.md for credentials, costs, and setup.
#[path = "support/datadog.rs"]
mod datadog;
use typesafe_ai::{choice, questions, SystemOneTask, TypeSafeClient};

fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let api_key = std::env::var("TYPESAFE_API_KEY").map_err(|_| "TYPESAFE_API_KEY is required")?;
    let telemetry = datadog::Telemetry::from_env()?;
    let _subscriber = tracing::dispatcher::set_default(&telemetry.dispatch);
    let runtime = tokio::runtime::Runtime::new()?;
    let result = runtime.block_on(async {
        let client = TypeSafeClient::builder()
            .api_key(api_key)
            .meter(telemetry.meter())
            .build()?;
        let task = SystemOneTask::builder()
            .model(std::env::var("TYPESAFE_MODEL").unwrap_or_else(|_| "jev-1.13.0".into()))
            .questions(
                questions! { action: choice("Admit this request or defer it?",
                [("admit", "Capacity is available"), ("defer", "Wait for capacity")]), },
            )
            .build()?;
        let response = client
            .system_one(&task, &serde_json::json!({"active":2,"capacity":10}))
            .await?;
        println!(
            "Action: {}; resolved model: {}",
            response.answers.action.choice, response.model
        );
        Ok::<_, typesafe_ai::Error>(())
    });
    // A successful call still produces an operational log so all three export paths can be checked.
    tracing::info!(target: "typesafe_ai::example", successful = result.is_ok(), "TypeSafe example finished");
    drop(_subscriber);
    let export_result = telemetry.shutdown();
    // Display only a stable category: custom serialization errors may contain application data.
    if let Err(error) = result {
        return Err(format!("TypeSafe call failed: {}", error.code()).into());
    }
    export_result
}
