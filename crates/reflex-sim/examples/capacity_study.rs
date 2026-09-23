// Unless explicitly stated otherwise all files in this repository are licensed under the Apache-2.0 License.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-present Datadog, Inc.

//! Paired, headless capacity experiments. See studies/capacity/PROTOCOL.md.
#[path = "support/performance.rs"]
mod performance;

use clap::Parser;
use reflex::Controller;
use reflex_sim::capacity::{
    engine::{Action, Engine, Event, JobPhase, Lifecycle, Proposal, Settings},
    forecast::{self, Snapshot},
    judge::{self, Evidence, Inference},
    workload::{Arrival, Bucket},
};
use reflex_typesafe::TypeSafeJudge;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::{BufWriter, Write},
    path::PathBuf,
    time::{Duration, Instant},
};
use typesafe_ai::{choice, questions, SystemOneTask, TypeSafeClient};

type StudyResult<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
const PROMPT: &str = "Choose a capacity action. Primary objective: complete offered jobs within estimated duration plus 20 seconds of arrival; minimize active node-seconds subject to this objective. Rejected jobs fail the objective. Each node has 8 CPU and 16 GiB, jobs reserve both until completion. FIFO first-fit placement is fixed. Starting nodes cost resources but cannot accept jobs until startup_s; draining nodes finish existing jobs and accept no new jobs. Account for starting capacity. Recent_history contains offered jobs/s, CPU-seconds/s and GiB-seconds/s averaged in time buckets, oldest first. Forecasts, when present, are uncertain pointwise p10/p50/p90 demand estimates, not guarantees. Anticipate startup delays, avoid unnecessary starts and oscillation, keep at least one ready node. Select only a legal choice. No future arrivals or true job durations are provided.";
#[derive(Parser, Clone, Serialize)]
struct Args {
    #[arg(long, default_value = "output/capacity-study")]
    output: PathBuf,
    #[arg(long, default_value_t = 10)]
    seeds: u64,
    #[arg(long, default_value_t = 1001)]
    first_seed: u64,
    #[arg(long, default_value_t = 1800)]
    duration: u64,
    #[arg(long, default_value_t = 300)]
    warmup: u64,
    #[arg(long, default_value_t = 60)]
    startup: u64,
    #[arg(long, default_value = "steady,spike,ramp,waves,bursts,mix")]
    patterns: String,
    #[arg(long, default_value = "moderate,heavy")]
    loads: String,
    #[arg(
        long,
        default_value = "fixed2,fixed6,reactive,hpa,ewma,persistence,toto_threshold,jev,jev_toto"
    )]
    policies: String,
    /// Use supplied forecast recordings, or run without forecast-enabled policies.
    #[arg(long, default_value = "none", value_parser = ["none", "cache"])]
    forecasts: String,
    #[arg(long, default_value = "jev-1.13.0")]
    model: String,
    /// Optional fixed instruction for a separately recorded prompt experiment.
    #[arg(long)]
    #[serde(skip_serializing_if = "Option::is_none")]
    prompt_file: Option<PathBuf>,
    #[arg(long, default_value_t = 0.8)]
    target: f64,
    #[arg(long, default_value_t = 60)]
    downscale_wait: u64,
    #[arg(long, default_value_t = 0.2)]
    ewma_alpha: f64,
    #[arg(long, default_value = "equal")]
    timing: String,
    #[arg(long, default_value_t = 0.10)]
    node_hour_usd: f64,
    #[arg(long)]
    replay: bool,
    #[arg(long)]
    tuning: Option<PathBuf>,
    /// Include two observed 60-second performance windows in Jev evidence.
    #[arg(long)]
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    performance_feedback: bool,
}
#[derive(Clone, Serialize, Deserialize)]
struct ForecastRecord {
    at: u64,
    available_at: u64,
    input: forecast::Input,
    snapshot: Option<Snapshot>,
    error: Option<String>,
    latency_ms: f64,
}
#[derive(Clone, Serialize, Deserialize)]
struct Decision {
    at: u64,
    due: u64,
    evidence: Value,
    result: Inference,
    action: Action,
    forecast_origin: Option<u64>,
    fallback: bool,
}
struct Lane {
    name: String,
    engine: Engine,
    decisions: BufWriter<File>,
    timeline: Vec<Value>,
    pending: Option<Decision>,
    smoothed: [f64; 3],
    desired_history: Vec<(u64, usize)>,
    replay: BTreeMap<u64, Decision>,
    costs: Costs,
    guards: u64,
    fallbacks: u64,
    deterministic: u64,
    latencies: Vec<f64>,
    performance: performance::Performance,
    node_seconds: u64,
    used_cpu_seconds: u64,
    used_mem_seconds: u64,
    ready_seconds: u64,
    done: bool,
}
#[derive(Default, Serialize)]
struct Costs {
    calls: u64,
    input_tokens: u64,
    output_tokens: u64,
    priced_calls: u64,
    missing_usage_calls: u64,
    unpriced_calls: u64,
    estimated_usd: f64,
}
fn draw(seed: u64, t: i64, stream: u64) -> f64 {
    let mut x = seed
        ^ (t as u64).wrapping_mul(0x9e3779b97f4a7c15)
        ^ stream.wrapping_mul(0x94d049bb133111eb);
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d049bb133111eb);
    ((x ^ (x >> 31)) >> 11) as f64 / (1u64 << 53) as f64
}
fn multiplier(pattern: &str, t: i64) -> f64 {
    let t = t as f64;
    match pattern {
        "steady" | "mix" => 1.,
        "spike" => {
            if (600. ..900.).contains(&t) {
                2.5
            } else {
                0.8
            }
        }
        "ramp" => {
            if t < 300. {
                0.7
            } else if t < 900. {
                0.7 + (t - 300.) / 600. * 1.5
            } else if t < 1200. {
                2.2
            } else if t < 1650. {
                2.2 - (t - 1200.) / 450. * 1.5
            } else {
                0.7
            }
        }
        "waves" => 1.3 + 0.7 * (t / 300. * std::f64::consts::TAU).sin(),
        "bursts" => {
            if t >= 0. && t % 240. < 40. {
                3.0
            } else {
                0.7
            }
        }
        _ => panic!("unknown pattern"),
    }
}
fn bucket(pattern: &str, load: &str, seed: u64, t: i64, warmup: u64) -> Bucket {
    let relative = t - warmup as i64;
    let mult = multiplier(pattern, relative) * if load == "heavy" { 1.5 } else { 1. };
    let mut jobs = vec![];
    let mut values = [0.; 3];
    for (c, (rate, mut cpu, mut mem, duration)) in
        [(0.35, 1, 1, 6), (0.16, 4, 2, 12), (0.14, 1, 8, 18)]
            .into_iter()
            .enumerate()
    {
        if pattern == "mix" && (600..1200).contains(&relative) {
            std::mem::swap(&mut cpu, &mut mem);
            cpu = cpu.min(8);
            mem = mem.min(16);
        }
        // Inverse CDF Poisson: unlike the playground's rounded arrivals, genuine variance at every rate.
        let u = draw(seed, t, c as u64 * 100);
        let lambda = rate * mult;
        let mut p = (-lambda).exp();
        let mut cumulative = p;
        let mut n = 0;
        while u > cumulative && n < 64 {
            n += 1;
            p *= lambda / n as f64;
            cumulative += p;
        }
        for j in 0..n {
            let actual = ((duration as f64 * (0.75 + 0.5 * draw(seed, t, c as u64 * 100 + j + 1)))
                .ceil() as u64)
                .max(1);
            jobs.push(Arrival {
                id: ((t + 10000) as u64) * 1000 + c as u64 * 100 + j,
                client: c,
                cpu,
                memory_gib: mem,
                estimated_s: duration,
                actual_s: actual,
            });
            values[0] += 1.;
            values[1] += (cpu as u64 * duration) as f32;
            values[2] += (mem as u64 * duration) as f32;
        }
    }
    Bucket {
        at_ms: t * 1000,
        values,
        jobs,
    }
}
fn write_json(path: impl AsRef<std::path::Path>, v: &impl Serialize) -> StudyResult<()> {
    let mut f = BufWriter::new(File::create(path)?);
    serde_json::to_writer(&mut f, v)?;
    f.flush()?;
    Ok(())
}
fn record(w: &mut BufWriter<File>, v: &impl Serialize) -> StudyResult<()> {
    serde_json::to_writer(&mut *w, v)?;
    w.write_all(b"\n")?;
    w.flush()?;
    Ok(())
}
fn recent(history: &[Bucket]) -> Vec<Value> {
    history
        .chunks(16)
        .map(|rows| {
            let mut mean = [0.; 3];
            for b in rows {
                for (i, v) in b.values.iter().enumerate() {
                    mean[i] += *v as f64 / rows.len() as f64;
                }
            }
            json!({"from_ms":rows[0].at_ms,"through_ms":rows.last().unwrap().at_ms,"mean":mean})
        })
        .collect()
}
fn action_for(e: &Evidence, desired: usize, wait: u64) -> Action {
    let planned = e.ready + e.starting;
    let a = if desired >= planned + 2 {
        Action::StartTwo
    } else if desired > planned {
        Action::StartOne
    } else if desired < e.ready
        && e.starting == 0
        && e.queued == 0
        && e.last_change_age_ms.is_none_or(|t| t >= wait * 1000)
    {
        Action::DrainOne
    } else {
        Action::Hold
    };
    if e.legal_choices.contains(&a) {
        a
    } else if a == Action::StartTwo && e.legal_choices.contains(&Action::StartOne) {
        Action::StartOne
    } else {
        Action::Hold
    }
}
fn classical(
    name: &str,
    e: &Evidence,
    smoothed: &mut [f64; 3],
    desired_history: &mut Vec<(u64, usize)>,
    args: &Args,
) -> Action {
    if name == "fixed2" {
        return Action::Hold;
    }
    if name == "fixed6" {
        return action_for(e, 6, 0);
    }
    let mut demand = e.observed_demand_10s;
    for i in 0..3 {
        smoothed[i] = args.ewma_alpha * demand[i] + (1. - args.ewma_alpha) * smoothed[i];
    }
    if name == "ewma" {
        demand = *smoothed;
    }
    if name == "toto_threshold" {
        if let Some(f) = &e.forecast {
            for b in &f.buckets {
                demand[1] = demand[1].max(b.mean_p90[1]);
                demand[2] = demand[2].max(b.mean_p90[2]);
            }
        }
    }
    // Persistence forecasts the last observed demand; its max-with-observed rule is intentionally identical to reactive.
    let mut desired = ((demand[1] / 8.).max(demand[2] / 16.) / args.target).ceil() as usize;
    if name == "hpa" {
        let used_cpu = e
            .nodes
            .iter()
            .filter(|n| matches!(n.phase, Lifecycle::Ready | Lifecycle::Draining))
            .map(|n| 8 - n.free_cpu)
            .sum::<u32>() as f64;
        let used_mem = e
            .nodes
            .iter()
            .filter(|n| matches!(n.phase, Lifecycle::Ready | Lifecycle::Draining))
            .map(|n| 16 - n.free_memory_gib)
            .sum::<u32>() as f64;
        desired = ((used_cpu / 8.).max(used_mem / 16.) / args.target).ceil() as usize;
        if e.ready > 0
            && ((used_cpu / (8. * e.ready as f64)).max(used_mem / (16. * e.ready as f64))
                / args.target
                - 1.)
                .abs()
                < 0.1
        {
            desired = e.ready;
        }
        desired_history.push((e.observed_at_ms, desired));
        desired_history.retain(|(t, _)| e.observed_at_ms - *t <= args.downscale_wait * 1000);
        if desired < e.ready {
            desired = desired_history
                .iter()
                .map(|(_, n)| *n)
                .max()
                .unwrap_or(desired);
        }
    }
    if e.queued > 0 {
        desired = desired.max(e.ready + 1);
    }
    action_for(e, desired.clamp(1, e.max_nodes), args.downscale_wait)
}
async fn jev(
    client: &TypeSafeClient,
    state: &Value,
    e: &Evidence,
    model: &str,
    prompt: &str,
) -> Inference {
    let start = Instant::now();
    let task=SystemOneTask::builder().model(model).questions(questions!{intervention:choice(prompt,e.legal_choices.iter().copied().map(|a|(a,a.label())).collect::<Vec<_>>())}).build().unwrap();
    let judge = TypeSafeJudge::new(client.clone(), task).select_answer(|a| a.intervention);
    let controller = Controller::builder()
        .judge(judge)
        .inference_timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let mut r = match controller.evaluate(state).await {
        Ok(p) => {
            let info = controller.judge().last_response();
            Inference {
                choice: Some(*p.action()),
                confidence: p.confidence(),
                probabilities: info
                    .as_ref()
                    .map(|d| d.probabilities.clone())
                    .unwrap_or_default(),
                model: info.as_ref().map(|d| d.model.clone()),
                usage: info.map(|d| d.usage),
                latency_ms: 0.,
                error: None,
            }
        }
        Err(e) => Inference::failed(e.to_string()),
    };
    r.latency_ms = start.elapsed().as_secs_f64() * 1000.;
    r
}
fn percentile(v: &[f64], p: f64) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    let mut v = v.to_vec();
    v.sort_by(f64::total_cmp);
    Some(v[((v.len() - 1) as f64 * p).ceil() as usize])
}
fn summary(l: &Lane, args: &Args, pattern: &str, load: &str, seed: u64) -> Value {
    let d = l.engine.data();
    let jobs: Vec<_> = d
        .jobs
        .iter()
        .filter(|j| {
            j.arrived_at > args.warmup * 1000
                && j.arrived_at <= (args.warmup + args.duration) * 1000
        })
        .collect();
    let mut waits = vec![];
    let mut latency = vec![];
    let mut good = 0;
    let mut rejected = 0;
    let mut unfinished = 0;
    for j in &jobs {
        if j.phase == JobPhase::Completed {
            let wait = (j.started_at.unwrap() - j.arrived_at) as f64 / 1000.;
            let elapsed = wait + j.arrival.actual_s as f64;
            waits.push(wait);
            latency.push(elapsed);
            if elapsed <= j.arrival.estimated_s as f64 + 20. {
                good += 1;
            }
        } else if j.phase == JobPhase::Rejected {
            rejected += 1;
        } else {
            unfinished += 1;
        }
    }
    let changes = d
        .changes
        .iter()
        .filter(|c| c.at_ms > args.warmup * 1000)
        .count();
    json!({"pattern":pattern,"load":load,"seed":seed,"policy":l.name,"timing":args.timing,"offered":jobs.len(),"slo_success":good,"slo_rate":good as f64/jobs.len().max(1) as f64,"completed":latency.len(),"rejected":rejected,"unfinished":unfinished,"wait_p50_s":percentile(&waits,0.5),"wait_p95_s":percentile(&waits,0.95),"wait_p99_s":percentile(&waits,0.99),"latency_p95_s":percentile(&latency,0.95),"node_seconds":l.node_seconds,"node_cost_usd":l.node_seconds as f64/3600.*args.node_hour_usd,"used_cpu_seconds":l.used_cpu_seconds,"used_memory_gib_seconds":l.used_mem_seconds,"ready_node_seconds":l.ready_seconds,"cpu_reservation_utilization":l.used_cpu_seconds as f64/(8*l.node_seconds).max(1) as f64,"memory_reservation_utilization":l.used_mem_seconds as f64/(16*l.node_seconds).max(1) as f64,"capacity_changes":changes,"guard_rejections":l.guards,"fallbacks":l.fallbacks,"deterministic_decisions":l.deterministic,"inference_latency_p50_ms":percentile(&l.latencies,0.5),"inference_latency_p95_ms":percentile(&l.latencies,0.95),"jev_cost":l.costs,"known_combined_cost_usd":l.node_seconds as f64/3600.*args.node_hour_usd+l.costs.estimated_usd,"toto_cost_usd":null})
}
#[tokio::main]
async fn main() -> StudyResult<()> {
    let args = Args::parse();
    let prompt = if let Some(path) = &args.prompt_file {
        fs::read_to_string(path)?
    } else {
        PROMPT.to_owned()
    };
    if prompt.trim().is_empty() {
        return Err("Prompt must not be empty".into());
    }
    let stop_file = args.output.join("STOP.json");
    if !args.replay && stop_file.exists() {
        return Err("Study stopped by a billing/authentication error. Resolve it and archive STOP.json before resuming.".into());
    }
    if !["none", "cache"].contains(&args.forecasts.as_str())
        || !["equal", "measured"].contains(&args.timing.as_str())
    {
        return Err("Invalid forecasts/timing".into());
    }
    if args.duration == 0
        || args.warmup < 1
        || args.target <= 0.
        || args.target > 1.
        || !(0. ..=1.).contains(&args.ewma_alpha)
    {
        return Err("Invalid experiment configuration".into());
    }
    let names: Vec<_> = args.policies.split(',').collect();
    for n in &names {
        if ![
            "fixed2",
            "fixed6",
            "reactive",
            "hpa",
            "ewma",
            "persistence",
            "toto_threshold",
            "jev",
            "jev_toto",
        ]
        .contains(n)
        {
            return Err(format!("Unknown policy {n}").into());
        }
    }
    let uses_toto = names.iter().any(|n| n.contains("toto"));
    if uses_toto && args.forecasts == "none" {
        return Err("Toto arms require --forecasts cache and supplied forecast recordings".into());
    }
    fs::create_dir_all(&args.output)?;
    // OS-owned lock prevents duplicate paid runs when independent workers meet on resume.
    let run_lock = if !args.replay {
        let file = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(
                args.output
                    .join(format!("worker-{}-{}.lock", args.first_seed, args.timing)),
            )?;
        file.lock()?;
        Some(file)
    } else {
        None
    };
    let _run_lock = run_lock;
    let call_spacing = std::env::var("REFLEX_STUDY_MIN_CALL_MS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(300)
        .max(300);
    let manifest = json!({"version":1,"args":args,"prompt":prompt,"price":{"model":"jev-1.13.0","input_usd_per_million":0.042,"output_usd_per_million":0,"verified_date":"2026-09-20","source":"https://docs.typesafe.ai/models"},"toto_price":null,"cost_scope":"All calls including warmup; node cost measurement plus completion period only","slo":"completion <= estimated_duration + 20 seconds after arrival","history":"256 seconds, summarized into 16-second means for Jev; raw 1s series to Toto","equal_timing":"1 simulated second for every decision and forecast","measured_timing":"ceil measured RPC latency to 1s ticks; classical minimum 1s","completion_period_max_s":120});
    let manifest_path = args
        .output
        .join(format!("manifest-{}-{}.json", args.first_seed, args.timing));
    if !args.replay && manifest_path.exists() {
        let old: Value = serde_json::from_reader(File::open(&manifest_path)?)?;
        if old != manifest {
            return Err("Manifest differs: use a new output directory".into());
        }
    }
    if !args.replay {
        write_json(manifest_path, &manifest)?;
    }
    let tuning: Value = if let Some(p) = &args.tuning {
        serde_json::from_reader(File::open(p)?)?
    } else {
        json!({})
    };
    let client = if names.iter().any(|n| n.starts_with("jev")) && !args.replay {
        Some(
            TypeSafeClient::builder()
                .api_key(std::env::var("TYPESAFE_API_KEY")?)
                .timeout(Duration::from_secs(5))
                .max_retries(0)
                .build()?,
        )
    } else {
        None
    };
    for seed in args.first_seed..args.first_seed + args.seeds {
        for pattern in args.patterns.split(',') {
            for load in args.loads.split(',') {
                if !["steady", "spike", "ramp", "waves", "bursts", "mix"].contains(&pattern)
                    || !["moderate", "heavy"].contains(&load)
                {
                    return Err("Invalid workload".into());
                }
                let dir = args.output.join(format!("{pattern}-{load}-{seed}"));
                fs::create_dir_all(&dir)?;
                let result_path = dir.join(format!("results-{}.json", args.timing));
                if !args.replay && result_path.exists() {
                    println!("SKIP {}", dir.display());
                    continue;
                }
                let trace: Vec<Bucket> = (-255..=(args.warmup + args.duration) as i64)
                    .map(|t| bucket(pattern, load, seed, t, args.warmup))
                    .collect();
                if !args.replay {
                    write_json(dir.join("workload.json"), &trace)?;
                }
                let forecasts: Vec<ForecastRecord> = if dir.join("forecasts.json").exists() {
                    serde_json::from_reader(File::open(dir.join("forecasts.json"))?)?
                } else {
                    vec![]
                };
                if uses_toto && forecasts.is_empty() {
                    return Err("Missing recorded forecasts".into());
                }
                let mut lanes = vec![];
                for name in &names {
                    let replay = if args.replay {
                        let text = fs::read_to_string(
                            dir.join(format!("decisions-{name}-{}.jsonl", args.timing)),
                        )?;
                        text.lines()
                            .map(|s| {
                                let d: Decision = serde_json::from_str(s).unwrap();
                                (d.at, d)
                            })
                            .collect()
                    } else {
                        BTreeMap::new()
                    };
                    let suffix = if args.replay {
                        "replayed"
                    } else {
                        &args.timing
                    };
                    lanes.push(Lane {
                        name: name.to_string(),
                        engine: Engine::new(Settings {
                            startup_s: args.startup,
                            max_nodes: 6,
                        })?,
                        decisions: BufWriter::new(File::create(
                            dir.join(format!("decisions-{name}-{suffix}.jsonl")),
                        )?),
                        timeline: vec![],
                        pending: None,
                        smoothed: [0.; 3],
                        desired_history: vec![],
                        replay,
                        costs: Costs::default(),
                        guards: 0,
                        fallbacks: 0,
                        deterministic: 0,
                        latencies: vec![],
                        performance: performance::Performance::default(),
                        node_seconds: 0,
                        used_cpu_seconds: 0,
                        used_mem_seconds: 0,
                        ready_seconds: 0,
                        done: false,
                    });
                }
                for t in 0..=args.warmup + args.duration + 120 {
                    if !args.replay && stop_file.exists() {
                        return Err(
                            "Another study worker stopped on billing/authentication failure".into(),
                        );
                    }
                    let end = args.warmup + args.duration;
                    if t > 0 {
                        let b = if t <= end {
                            trace[(t + 255) as usize].clone()
                        } else {
                            Bucket {
                                at_ms: t as i64 * 1000,
                                values: [0.; 3],
                                jobs: vec![],
                            }
                        };
                        for l in &mut lanes {
                            if l.done {
                                continue;
                            }
                            let before = l.engine.data();
                            if t > args.warmup {
                                l.node_seconds += before.active() as u64;
                                l.used_cpu_seconds +=
                                    before.nodes.iter().map(|n| n.used_cpu as u64).sum::<u64>();
                                l.used_mem_seconds += before
                                    .nodes
                                    .iter()
                                    .map(|n| n.used_memory_gib as u64)
                                    .sum::<u64>();
                                l.ready_seconds += before.count(Lifecycle::Ready) as u64;
                            }
                            l.engine.event(Event::Tick(b.clone())).await?;
                            if args.performance_feedback {
                                l.performance.observe(&before, &l.engine.data());
                            }
                            if let Some(p) = l.pending.take() {
                                if p.due <= t {
                                    let current = l.engine.data();
                                    let e: &Value = &p.evidence["current"];
                                    let proposal = Proposal {
                                        action: p.action,
                                        observed_at: e["observed_at_ms"].as_u64().unwrap(),
                                        revision: e["revision"].as_u64().unwrap(),
                                        forecast_origin: p.forecast_origin,
                                    };
                                    let (applied, reason) =
                                        l.engine.apply(proposal, p.result.confidence).await?;
                                    if !applied {
                                        l.guards += 1;
                                    }
                                    l.timeline.push(json!({"at":t,"decision_at":p.at,"applied":applied,"reason":reason,"action":p.action,"revision":current.revision}));
                                } else {
                                    if !args.replay
                                        && p.result.error.as_deref().is_some_and(|e| {
                                            e.contains("typesafe_http_402")
                                                || e.contains("typesafe_http_401")
                                                || e.contains("typesafe_http_403")
                                        })
                                    {
                                        write_json(
                                            &stop_file,
                                            &json!({"reason":p.result.error,"pattern":pattern,"load":load,"seed":seed,"policy":l.name,"at":t}),
                                        )?;
                                        return Err("Billing/authentication failure: run excluded; all workers stopped".into());
                                    }
                                    l.pending = Some(p);
                                }
                            }
                            if t >= end
                                && l.engine.data().running() == 0
                                && l.engine.data().queued() == 0
                            {
                                l.done = true;
                            }
                        }
                    }
                    if t % 10 != 0 {
                        continue;
                    }
                    let h: Vec<_> = trace
                        .iter()
                        .filter(|b| b.at_ms <= t as i64 * 1000)
                        .rev()
                        .take(256)
                        .cloned()
                        .collect::<Vec<_>>()
                        .into_iter()
                        .rev()
                        .collect();
                    let latest = forecasts
                        .iter()
                        .rfind(|f| {
                            let delay = if args.timing == "measured" {
                                (f.latency_ms / 1000.).ceil().max(1.) as u64
                            } else {
                                1
                            };
                            f.at + delay <= t && f.snapshot.is_some()
                        })
                        .and_then(|f| f.snapshot.as_ref())
                        .filter(|s| s.fresh(t * 1000));
                    for l in &mut lanes {
                        if l.done {
                            continue;
                        }
                        let d = l.engine.data();
                        l.timeline.push(json!({"at":t,"queued":d.queued(),"running":d.running(),"ready":d.count(Lifecycle::Ready),"starting":d.count(Lifecycle::Starting),"draining":d.count(Lifecycle::Draining),"offered":d.offered,"node_seconds":l.node_seconds}));
                        if t >= end || l.pending.is_some() {
                            continue;
                        }
                        let e = judge::evidence(
                            &d,
                            &h,
                            if l.name.contains("toto") {
                                latest
                            } else {
                                None
                            },
                        );
                        let mut state = json!({"current":e,"recent_history":recent(&h)});
                        if args.performance_feedback {
                            state["performance"] = l.performance.evidence(&d);
                        }
                        let mut policy_args = args.clone();
                        let key = if l.name == "persistence"
                            || l.name == "toto_threshold"
                            || l.name.starts_with("jev")
                        {
                            "reactive"
                        } else {
                            l.name.as_str()
                        };
                        if let Some(v) = tuning.get(key) {
                            policy_args.target = v["target"].as_f64().unwrap();
                            policy_args.downscale_wait = v["downscale_wait"].as_u64().unwrap();
                            policy_args.ewma_alpha = v["ewma_alpha"].as_f64().unwrap();
                        }
                        let p = if args.replay {
                            let p = l.replay.remove(&t).ok_or("Replay missing decision")?;
                            if p.evidence != state {
                                return Err(format!(
                                    "Replay evidence diverged {pattern}/{load}/{seed}/{} at {t}",
                                    l.name
                                )
                                .into());
                            }
                            p
                        } else {
                            let mut result = Inference::failed("");
                            result.error = None;
                            let mut fallback = false;
                            let a = if l.name.starts_with("jev") && e.legal_choices.len() > 1 {
                                let start = Instant::now();
                                result =
                                    jev(client.as_ref().unwrap(), &state, &e, &args.model, &prompt)
                                        .await;
                                // Four study processes together remain below the published 1200 RPM limit.
                                tokio::time::sleep(
                                    Duration::from_millis(call_spacing)
                                        .saturating_sub(start.elapsed()),
                                )
                                .await;
                                if let Some(a) = result.choice {
                                    a
                                } else {
                                    fallback = true;
                                    classical(
                                        "reactive",
                                        &e,
                                        &mut l.smoothed,
                                        &mut l.desired_history,
                                        &policy_args,
                                    )
                                }
                            } else if l.name.starts_with("jev") {
                                e.legal_choices[0]
                            } else {
                                classical(
                                    &l.name,
                                    &e,
                                    &mut l.smoothed,
                                    &mut l.desired_history,
                                    &policy_args,
                                )
                            };
                            if l.name.contains("toto") && e.forecast.is_none() {
                                fallback = true;
                            }
                            let delay = if args.timing == "measured" {
                                (result.latency_ms / 1000.).ceil().max(1.) as u64
                            } else {
                                1
                            };
                            Decision {
                                at: t,
                                due: t + delay,
                                evidence: state,
                                result,
                                action: a,
                                forecast_origin: e.forecast.as_ref().map(|f| f.origin_ms),
                                fallback,
                            }
                        };
                        if p.result.latency_ms > 0. {
                            l.costs.calls += 1;
                            l.latencies.push(p.result.latency_ms);
                            if let Some(u) = &p.result.usage {
                                l.costs.input_tokens += u.input_tokens;
                                l.costs.output_tokens += u.output_tokens;
                                if p.result.model.as_deref() == Some("jev-1.13.0") {
                                    l.costs.priced_calls += 1;
                                    l.costs.estimated_usd += u.input_tokens as f64 * 0.042 / 1e6;
                                } else {
                                    l.costs.unpriced_calls += 1;
                                }
                            } else {
                                l.costs.missing_usage_calls += 1;
                            }
                        } else {
                            l.deterministic += 1;
                        }
                        if p.fallback {
                            l.fallbacks += 1;
                        }
                        if !args.replay {
                            record(&mut l.decisions, &p)?;
                        }
                        if !args.replay
                            && p.result.error.as_deref().is_some_and(|e| {
                                e.contains("typesafe_http_402")
                                    || e.contains("typesafe_http_401")
                                    || e.contains("typesafe_http_403")
                            })
                        {
                            write_json(
                                &stop_file,
                                &json!({"reason":p.result.error,"pattern":pattern,"load":load,"seed":seed,"policy":l.name,"at":t}),
                            )?;
                            return Err(
                                "Billing/authentication failure: run excluded; all workers stopped"
                                    .into(),
                            );
                        }
                        l.pending = Some(p);
                    }
                }
                let mut results = vec![];
                for l in &lanes {
                    let s = summary(l, &args, pattern, load, seed);
                    println!("RESULT {pattern}/{load}/{seed}/{} SLO={:.3} node_s={} calls={} cost=${:.5}",l.name,s["slo_rate"].as_f64().unwrap(),l.node_seconds,l.costs.calls,l.costs.estimated_usd);
                    results.push(s);
                    if !args.replay {
                        write_json(
                            dir.join(format!("timeline-{}-{}.json", l.name, args.timing)),
                            &l.timeline,
                        )?;
                        write_json(
                            dir.join(format!("jobs-{}-{}.json", l.name, args.timing)),
                            &l.engine.data().jobs,
                        )?;
                    }
                }
                if args.replay {
                    let old: Value = serde_json::from_reader(File::open(&result_path)?)?;
                    if old != json!(results) {
                        return Err("Replay summary diverged".into());
                    }
                    println!("REPLAY VERIFIED {pattern}/{load}/{seed}");
                } else {
                    write_json(result_path, &results)?;
                }
            }
        }
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn future_does_not_change_history() {
        let a = bucket("spike", "moderate", 7, 300, 300);
        let b = bucket("spike", "moderate", 7, 300, 300);
        assert_eq!(a, b);
        assert_ne!(
            bucket("spike", "moderate", 8, 900, 300),
            bucket("spike", "moderate", 7, 900, 300)
        );
    }
    #[test]
    fn quantiles_and_null_samples() {
        assert_eq!(percentile(&[], 0.95), None);
        assert_eq!(percentile(&[1., 9., 3.], 0.5), Some(3.));
    }
    #[tokio::test]
    async fn accounting_and_future_are_hidden() {
        let mut engine = Engine::new(Settings::default()).unwrap();
        for t in 1..200 {
            engine
                .event(Event::Tick(bucket("steady", "moderate", 42, t, 10)))
                .await
                .unwrap();
        }
        let d = engine.data();
        reflex_sim::capacity::engine::invariant(&reflex_sim::capacity::engine::Phase::Managing, &d)
            .unwrap();
        assert_eq!(d.offered, d.jobs.len() as u64);
        let s = serde_json::to_string(&judge::evidence(&d, &[], None)).unwrap();
        assert!(!s.contains("actual_s"));
        assert!(!s.contains("finish_at"));
    }
}
