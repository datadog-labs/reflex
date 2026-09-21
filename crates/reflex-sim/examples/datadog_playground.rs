//! Time-bounded circuit-breaker, scheduler and recovery playgrounds with direct Datadog export.
//! Requires DD_API_KEY and TYPESAFE_API_KEY; accepts DD_SITE, DD_SERVICE, DD_ENV.
// Share the application exporter used by the standalone client example and its wire tests.
#[path = "../../typesafe-ai/examples/support/datadog.rs"]
mod datadog;

use clap::Parser;
use reflex_sim::{
    jev::{LiveEvaluator, PolicyKind},
    playground::{self, inference::JevSettings},
};
use std::{sync::Arc, time::Duration};

#[derive(Parser)]
struct Args {
    /// Query Datadog for circuit-breaker, scheduler and recovery evidence. Requires DD_APP_KEY.
    #[arg(long)]
    datadog_evidence: bool,
    #[arg(long, default_value_t = 8743)]
    port: u16,
    /// Stop the server and flush all telemetry after this many wall-clock seconds.
    #[arg(long, default_value_t = 120, value_parser = clap::value_parser!(u16).range(10..=600))]
    duration_secs: u16,
    #[arg(long, default_value_t = 60, value_parser = clap::value_parser!(u16).range(1..=100))]
    max_evaluations: u16,
}

fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let args = Args::parse();
    let key = std::env::var("TYPESAFE_API_KEY").map_err(|_| "TYPESAFE_API_KEY is required")?;
    let evidence_source = if args.datadog_evidence {
        Some(reflex_sim::datadog::Source::from_env()?)
    } else {
        None
    };
    let telemetry = datadog::Telemetry::from_env()?;
    telemetry.install_global()?;
    let runtime = tokio::runtime::Runtime::new()?;
    let result = runtime.block_on(async {
        let model = std::env::var("TYPESAFE_MODEL").unwrap_or_else(|_| "jev-1.13.0".into());
        let client = typesafe_ai::TypeSafeClient::builder()
            .api_key(key)
            .meter(telemetry.meter())
            .timeout(Duration::from_secs(2))
            .max_retries(0)
            .build()?;
        let settings = JevSettings {
            model: model.clone(),
            max_evaluations: usize::from(args.max_evaluations),
            ..Default::default()
        };
        let mut scheduler: Arc<dyn reflex_sim::scheduler::judge::Evaluator> = Arc::new(
            reflex_sim::scheduler::judge::LiveEvaluator::new(client.clone(), model.clone()),
        );
        let mut recovery: Arc<dyn reflex_sim::recovery::judge::Evaluator> = Arc::new(
            reflex_sim::recovery::judge::LiveEvaluator::new(client.clone(), model.clone()),
        );
        let mut breaker: Arc<dyn reflex_sim::jev::Evaluator> =
            Arc::new(LiveEvaluator::new(client, model));
        if args.datadog_evidence {
            recovery = Arc::new(reflex_sim::recovery::datadog::DatadogEvaluator::new(
                recovery,
                reflex_sim::datadog::Source::from_env()?,
            ));
            scheduler = Arc::new(reflex_sim::scheduler::datadog::DatadogEvaluator::new(
                scheduler,
                reflex_sim::datadog::Source::from_env()?,
            ));
        }
        if let Some(source) = evidence_source {
            breaker = Arc::new(reflex_sim::datadog::DatadogEvaluator::new(breaker, source));
        }
        tracing::info!(target: "reflex_sim::playground", "Datadog playground started");
        tokio::select! {
            result = playground::serve_with_forecasts(42, args.port, false, PolicyKind::Jev,
                Some(breaker), settings, Some(scheduler), Some(recovery), None) => result?,
            _ = tokio::time::sleep(Duration::from_secs(u64::from(args.duration_secs))) => {}
        }
        Ok::<_, Box<dyn std::error::Error + Send + Sync>>(())
    });
    // Stop simulation tasks before flushing their final spans, logs and counters.
    drop(runtime);
    tracing::info!(target: "reflex_sim::playground", successful = result.is_ok(), "Datadog playground finished");
    let export_result = telemetry.shutdown();
    result?;
    export_result?;
    println!("Datadog metrics, traces and logs flushed successfully.");
    Ok(())
}
