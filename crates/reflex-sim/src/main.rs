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
    /// Launch the interactive incident playground instead of generating a report.
    #[arg(long, conflicts_with_all = ["scenario", "scenario_file", "algorithms", "output", "list_scenarios"])]
    playground: bool,
    /// Initial playground policy. Switching policy starts a fresh incident.
    #[arg(long, value_enum, default_value_t = reflex_sim::jev::PolicyKind::Threshold, requires = "playground")]
    policy: reflex_sim::jev::PolicyKind,
    /// TypeSafe model used by the Jev policy.
    #[arg(long, default_value = "jev-1.13.0", requires = "playground")]
    jev_model: String,
    /// Maximum live evaluations per incident (replay makes no API calls).
    #[arg(long, default_value_t = 180, value_parser = clap::value_parser!(u16).range(1..=1000), requires = "playground")]
    jev_max_evaluations: u16,
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
#[tokio::main]
async fn main() {
    if let Err(error) = run(Args::parse()).await {
        eprintln!("Error: {error}");
        std::process::exit(1);
    }
}
async fn run(args: Args) -> Result<(), Error> {
    if args.playground {
        let settings = reflex_sim::playground::inference::JevSettings {
            model: args.jev_model.clone(),
            max_evaluations: args.jev_max_evaluations as usize,
            ..Default::default()
        };
        let mut recovery_evaluator: Option<
            std::sync::Arc<dyn reflex_sim::recovery::judge::Evaluator>,
        > = None;
        let mut scheduler_evaluator: Option<
            std::sync::Arc<dyn reflex_sim::scheduler::judge::Evaluator>,
        > = None;
        let evaluator: Option<std::sync::Arc<dyn reflex_sim::jev::Evaluator>> =
            match std::env::var("TYPESAFE_API_KEY")
                .ok()
                .filter(|key| !key.trim().is_empty())
            {
                Some(key) => {
                    let client = typesafe_ai::TypeSafeClient::builder()
                        .api_key(key)
                        .timeout(std::time::Duration::from_secs(2))
                        .max_retries(0)
                        .build()
                        .map_err(|e| Error::Invalid(e.to_string()))?;
                    recovery_evaluator = Some(std::sync::Arc::new(
                        reflex_sim::recovery::judge::LiveEvaluator::new(
                            client.clone(),
                            args.jev_model.clone(),
                        ),
                    ));
                    scheduler_evaluator = Some(std::sync::Arc::new(
                        reflex_sim::scheduler::judge::LiveEvaluator::new(
                            client.clone(),
                            args.jev_model.clone(),
                        ),
                    ));
                    Some(std::sync::Arc::new(reflex_sim::jev::LiveEvaluator::new(
                        client,
                        args.jev_model,
                    )))
                }
                None => None,
            };
        return reflex_sim::playground::serve_with_forecasts(
            args.seed,
            args.port,
            !args.no_open,
            args.policy,
            evaluator,
            settings,
            scheduler_evaluator,
            recovery_evaluator,
            None,
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
