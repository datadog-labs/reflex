// Unless explicitly stated otherwise all files in this repository are licensed under the Apache-2.0 License.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-present Datadog, Inc.

#[cfg(feature = "datadog")]
#[allow(dead_code)]
#[path = "../../typesafe-ai/examples/support/datadog.rs"]
mod datadog_export;

use clap::Parser;
use reflex_sim::{
    generate_trace,
    policy::builtins,
    presets,
    report::{open_report, write_report, Report, ScenarioReport},
    simulate, Error, Scenario,
};
use std::path::PathBuf;
#[derive(Parser)]
#[command(
    name = "reflex-sim",
    version,
    about = "Run reproducible circuit-breaking experiments and open an offline HTML report"
)]
struct Args {
    /// Enable local Toto forecasts (start integrations/toto first).
    #[arg(long, requires = "playground")]
    toto_url: Option<String>,
    /// Publish metrics, traces, and logs directly to Datadog (requires --features datadog).
    #[arg(long, requires = "playground")]
    datadog: bool,
    /// Use queried Datadog telemetry as Jev evidence for all three playgrounds.
    /// Requires --datadog, --policy jev, and DD_APP_KEY.
    #[arg(long, requires = "datadog")]
    datadog_evidence: bool,
    /// Launch the interactive incident playground instead of generating a report.
    #[arg(long, conflicts_with_all = ["scenario", "scenario_file", "algorithms", "output", "list_scenarios"])]
    playground: bool,
    /// Circuit-breaker playground policy (Jev + Reflex only).
    #[arg(long, default_value = "jev", value_parser = ["jev"], requires = "playground")]
    policy: String,
    /// Model provider behind the Jev policy. `openai` uses the OpenAI Decisions API
    /// and OPENAI_API_KEY instead of TypeSafe and TYPESAFE_API_KEY.
    #[arg(long, value_enum, default_value_t, requires = "playground")]
    provider: reflex_sim::provider::Provider,
    /// Model used by the Jev policy [default: jev-1.13.0, or gpt-6-luna with --provider openai].
    #[arg(long = "model", visible_alias = "jev-model", requires = "playground")]
    jev_model: Option<String>,
    /// Loopback port for the playground (0 chooses an available port).
    #[arg(long, default_value_t = 8742, requires = "playground")]
    port: u16,
    /// Scenario id, or 'all' for the canned suite.
    #[arg(long, default_value = "all")]
    scenario: String,
    /// Read one custom Scenario JSON file instead of a preset.
    #[arg(long, conflicts_with_all = ["list_scenarios", "scenario"])]
    scenario_file: Option<PathBuf>,
    /// Seed for the immutable offered-traffic trace and request-level random draws.
    #[arg(long, default_value_t = 42)]
    seed: u64,
    /// Comma-separated policy ids, applied independently to the same trace.
    #[arg(long, value_delimiter = ',', default_value = "unprotected,threshold")]
    algorithms: Vec<String>,
    /// Directory for index.html and full results.json (overwritten on each run).
    #[arg(long, default_value = "output/circuit-lab")]
    output: PathBuf,
    /// Generate the report without launching the browser.
    #[arg(long)]
    no_open: bool,
    /// List the available canned scenarios and exit.
    #[arg(long)]
    list_scenarios: bool,
}
fn main() {
    if let Err(error) = start(Args::parse()) {
        eprintln!("Error: {error}");
        std::process::exit(1);
    }
}
fn start(args: Args) -> Result<(), Error> {
    if args.datadog_evidence && args.policy != "jev" {
        return Err(Error::Invalid(
            "--datadog-evidence requires --policy jev".into(),
        ));
    }
    #[cfg(not(feature = "datadog"))]
    if args.datadog {
        return Err(Error::Invalid(
            "Datadog export requires building with --features datadog".into(),
        ));
    }
    // Validate query credentials before starting exporters or accepting browser commands.
    let sources = if args.datadog_evidence {
        let variable = args.provider.key_variable();
        if std::env::var(variable)
            .ok()
            .is_none_or(|key| key.trim().is_empty())
        {
            return Err(Error::Invalid(format!(
                "{variable} is required for Datadog evidence"
            )));
        }
        Some([
            reflex_sim::datadog::Source::from_env()?,
            reflex_sim::datadog::Source::from_env()?,
            reflex_sim::datadog::Source::from_env()?,
        ])
    } else {
        None
    };
    // Blocking exporter clients must be constructed and shut down outside Tokio.
    #[cfg(feature = "datadog")]
    let telemetry = if args.datadog {
        let telemetry = datadog_export::Telemetry::from_env_with_service("reflex")
            .map_err(|e| Error::Invalid(e.to_string()))?;
        telemetry
            .install_global()
            .map_err(|e| Error::Invalid(e.to_string()))?;
        println!(
            "Datadog export enabled: metrics every 10s, plus traces and logs. State source: {}.",
            if args.datadog_evidence {
                "queried Datadog telemetry"
            } else {
                "local simulation"
            }
        );
        Some(telemetry)
    } else {
        None
    };
    let playground = args.playground;
    let runtime = tokio::runtime::Runtime::new()?;
    let result = runtime.block_on(async {
        if playground {
            tokio::select! {
                result = run(args, sources) => result,
                signal = tokio::signal::ctrl_c() => signal.map_err(Error::Io),
            }
        } else {
            run(args, sources).await
        }
    });
    // Drop simulation tasks before flushing final application telemetry.
    drop(runtime);
    #[cfg(feature = "datadog")]
    if let Some(telemetry) = telemetry {
        let flushed = telemetry
            .shutdown()
            .map_err(|e| Error::Invalid(e.to_string()));
        result?;
        flushed?;
        println!("Datadog metrics, traces and logs flushed successfully.");
        return Ok(());
    }
    result
}
async fn run(args: Args, sources: Option<[reflex_sim::datadog::Source; 3]>) -> Result<(), Error> {
    if args.playground {
        let forecaster = args
            .toto_url
            .as_deref()
            .map(|url| {
                reflex_sim::toto::LocalToto::new(url)
                    .map(|f| {
                        std::sync::Arc::new(f)
                            as std::sync::Arc<dyn reflex_sim::capacity::forecast::Forecaster>
                    })
                    .map_err(Error::Invalid)
            })
            .transpose()?;
        if forecaster.is_some() {
            println!(
                "Local Toto forecasting enabled for circuit breaker, scheduler and cluster autoscaler."
            );
        }
        let provider = args.provider;
        let model = args
            .jev_model
            .unwrap_or_else(|| provider.default_model().into());
        let settings = reflex_sim::playground::inference::JevSettings {
            model: model.clone(),
            ..Default::default()
        };
        let mut scheduler_evaluator: Option<
            std::sync::Arc<dyn reflex_sim::scheduler::judge::Evaluator>,
        > = None;
        let mut autoscaler_evaluator: Option<
            std::sync::Arc<dyn reflex_sim::autoscaler::judge::Evaluator>,
        > = None;
        let mut evaluator: Option<std::sync::Arc<dyn reflex_sim::jev::Evaluator>> =
            match std::env::var(provider.key_variable())
                .ok()
                .filter(|key| !key.trim().is_empty())
            {
                Some(key) => {
                    // One client per simulation: the same settings, named so that each
                    // simulation's request metrics can be told apart from the others'.
                    let client = |name| provider.client(&key, name).map_err(Error::Invalid);
                    autoscaler_evaluator = Some(std::sync::Arc::new(
                        reflex_sim::autoscaler::judge::LiveEvaluator::new(
                            client("cluster_autoscaler")?,
                            model.clone(),
                        ),
                    ));
                    scheduler_evaluator = Some(std::sync::Arc::new(
                        reflex_sim::scheduler::judge::LiveEvaluator::new(
                            client("resource_scheduler")?,
                            model.clone(),
                        ),
                    ));
                    Some(std::sync::Arc::new(reflex_sim::jev::LiveEvaluator::new(
                        client("circuit_breaker")?,
                        model,
                    )))
                }
                // The TypeSafe default reports its missing key when the playground starts.
                None if provider == reflex_sim::provider::Provider::Openai => {
                    return Err(Error::Invalid(
                        "set OPENAI_API_KEY before starting the playground".into(),
                    ));
                }
                None => None,
            };
        if let Some([breaker_source, scheduler_source, autoscaler_source]) = sources {
            autoscaler_evaluator = autoscaler_evaluator.map(|inner| {
                std::sync::Arc::new(reflex_sim::autoscaler::datadog::DatadogEvaluator::new(
                    inner,
                    autoscaler_source,
                )) as std::sync::Arc<dyn reflex_sim::autoscaler::judge::Evaluator>
            });
            evaluator = evaluator.map(|inner| {
                std::sync::Arc::new(reflex_sim::datadog::DatadogEvaluator::new(
                    inner,
                    breaker_source,
                )) as std::sync::Arc<dyn reflex_sim::jev::Evaluator>
            });
            scheduler_evaluator = scheduler_evaluator.map(|inner| {
                std::sync::Arc::new(reflex_sim::scheduler::datadog::DatadogEvaluator::new(
                    inner,
                    scheduler_source,
                )) as std::sync::Arc<dyn reflex_sim::scheduler::judge::Evaluator>
            });
        }
        return reflex_sim::playground::serve_with_autoscaler(
            args.seed,
            args.port,
            !args.no_open,
            reflex_sim::jev::PolicyKind::Jev,
            evaluator,
            settings,
            scheduler_evaluator,
            forecaster,
            autoscaler_evaluator,
        )
        .await;
    }
    let available = presets();
    if args.list_scenarios {
        for s in available {
            println!(
                "{:<16} {}\n                 {}",
                s.id, s.name, s.description
            );
        }
        return Ok(());
    }
    let scenarios: Vec<Scenario> = if let Some(path) = args.scenario_file {
        vec![serde_json::from_slice(&std::fs::read(path)?)?]
    } else if args.scenario == "all" {
        available
    } else {
        vec![available
            .into_iter()
            .find(|s| s.id == args.scenario)
            .ok_or_else(|| {
                Error::Invalid(format!(
                    "unknown scenario '{}'; use --list-scenarios",
                    args.scenario
                ))
            })?]
    };
    let algorithms = builtins();
    let mut selected = Vec::new();
    for id in &args.algorithms {
        if selected
            .iter()
            .any(|p: &reflex_sim::PolicyFactory| p.id == id)
        {
            return Err(Error::Invalid(format!("duplicate algorithm '{id}'")));
        }
        selected.push(*algorithms.iter().find(|p| p.id == id).ok_or_else(|| {
            Error::Invalid(format!(
                "unknown algorithm '{id}'; available: unprotected, threshold"
            ))
        })?);
    }
    if selected.is_empty() {
        return Err(Error::Invalid("select at least one algorithm".into()));
    }
    let mut report = Report {
        version: env!("CARGO_PKG_VERSION").into(),
        seed: args.seed,
        scenarios: vec![],
    };
    for scenario in scenarios {
        let trace = generate_trace(&scenario, args.seed)?;
        println!(
            "{} · {} requests · trace {}",
            scenario.name,
            trace.requests.len(),
            trace.fingerprint
        );
        let mut runs = Vec::new();
        for factory in &selected {
            let run = simulate(&scenario, &trace, *factory).await?;
            println!(
                "  {:<22} {:>5} successful  {:>5} errors  {:>5} timeouts  {:>5} shed",
                factory.name,
                run.metrics.counts.success,
                run.metrics.counts.error,
                run.metrics.counts.timeout,
                run.metrics.counts.shed
            );
            runs.push(run);
        }
        report.scenarios.push(ScenarioReport {
            scenario,
            trace_fingerprint: trace.fingerprint,
            offered_requests: trace.requests.len(),
            runs,
        });
    }
    let path = write_report(&report, &args.output)?;
    println!(
        "\nReport: {}\nData:   {}",
        path.display(),
        path.with_file_name("results.json").display()
    );
    if !args.no_open {
        open_report(&path)?;
    }
    Ok(())
}
