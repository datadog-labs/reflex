// Unless explicitly stated otherwise all files in this repository are licensed under the Apache-2.0 License.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-present Datadog, Inc.

use super::{
    engine::{self, Action, Data, Lifecycle},
    forecast::{Snapshot, SERIES},
    workload::Bucket,
};
use reflex::Controller;
use reflex_typesafe::TypeSafeJudge;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    future::Future,
    pin::Pin,
    time::{Duration, Instant},
};
use typesafe_ai::{choice, questions, SystemOneTask, TypeSafeClient, Usage};
#[derive(Clone, Serialize, Deserialize)]
pub struct ForecastEvidence {
    pub origin_ms: u64,
    pub age_ms: u64,
    pub source: String,
    pub model_provenance: String,
    pub interval_ms: u64,
    pub series_names: Vec<String>,
    pub quantiles: [f32; 3],
    pub buckets: Vec<ForecastBucket>,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct ForecastBucket {
    pub from_ms: u64,
    pub through_ms: u64,
    pub mean_p10: [f64; 3],
    pub mean_p50: [f64; 3],
    pub mean_p90: [f64; 3],
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Evidence {
    pub observed_at_ms: u64,
    pub revision: u64,
    pub queued: usize,
    pub running: usize,
    pub oldest_wait_ms: u64,
    pub ready: usize,
    pub starting: usize,
    pub draining: usize,
    pub nodes: Vec<NodeEvidence>,
    pub startup_s: u64,
    pub max_nodes: usize,
    pub observed_demand_10s: [f64; 3],
    pub last_change_age_ms: Option<u64>,
    pub legal_choices: Vec<Action>,
    pub forecast: Option<ForecastEvidence>,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct NodeEvidence {
    pub id: usize,
    pub phase: Lifecycle,
    pub ready_in_ms: Option<u64>,
    pub free_cpu: u32,
    pub free_memory_gib: u32,
}
pub fn evidence(d: &Data, h: &[Bucket], f: Option<&Snapshot>) -> Evidence {
    let rows: Vec<_> = h.iter().rev().take(10).collect();
    let mut demand = [0.; 3];
    for b in &rows {
        for (i, v) in demand.iter_mut().enumerate() {
            *v += b.values[i] as f64 / rows.len() as f64;
        }
    }
    let forecast = f.filter(|f| f.fresh(d.at_ms)).map(|f| {
        let start = ((d.at_ms - f.origin_ms) / 1000) as usize;
        let n = f.series[0].median.len();
        let mut buckets = vec![];
        for i in (start..n).step_by(10) {
            let end = (i + 10).min(n);
            let mut lo = [0.; 3];
            let mut mid = [0.; 3];
            let mut hi = [0.; 3];
            for k in 0..3 {
                for j in i..end {
                    lo[k] += f.series[k].lower[j].max(0.) as f64 / (end - i) as f64;
                    mid[k] += f.series[k].median[j].max(0.) as f64 / (end - i) as f64;
                    hi[k] += f.series[k].upper[j].max(0.) as f64 / (end - i) as f64;
                }
            }
            buckets.push(ForecastBucket {
                from_ms: f.origin_ms + (i as u64 + 1) * 1000,
                through_ms: f.origin_ms + end as u64 * 1000,
                mean_p10: lo,
                mean_p50: mid,
                mean_p90: hi,
            });
        }
        ForecastEvidence {
            origin_ms: f.origin_ms,
            age_ms: d.at_ms - f.origin_ms,
            source: f.source.clone(),
            model_provenance: f.model_provenance.clone(),
            interval_ms: 1000,
            series_names: SERIES.iter().map(|s| s.to_string()).collect(),
            quantiles: f.quantiles,
            buckets,
        }
    });
    Evidence {
        observed_at_ms: d.at_ms,
        revision: d.revision,
        queued: d.queued(),
        running: d.running(),
        oldest_wait_ms: d
            .jobs
            .iter()
            .filter(|j| j.phase == engine::JobPhase::Queued)
            .map(|j| d.at_ms - j.arrived_at)
            .max()
            .unwrap_or(0),
        ready: d.count(Lifecycle::Ready),
        starting: d.count(Lifecycle::Starting),
        draining: d.count(Lifecycle::Draining),
        nodes: d
            .nodes
            .iter()
            .map(|n| NodeEvidence {
                id: n.id,
                phase: n.phase,
                ready_in_ms: n.ready_at.map(|t| t.saturating_sub(d.at_ms)),
                free_cpu: n.cpu - n.used_cpu,
                free_memory_gib: n.memory_gib - n.used_memory_gib,
            })
            .collect(),
        startup_s: d.settings.startup_s,
        max_nodes: d.settings.max_nodes,
        observed_demand_10s: demand,
        last_change_age_ms: d.last_action.map(|t| d.at_ms - t),
        legal_choices: engine::actions(d),
        forecast,
    }
}
pub fn baseline(e: &Evidence) -> Action {
    let mut cpu = e.observed_demand_10s[1];
    let mut mem = e.observed_demand_10s[2];
    if let Some(f) = &e.forecast {
        for b in &f.buckets {
            cpu = cpu.max(b.mean_p90[1]);
            mem = mem.max(b.mean_p90[2]);
        }
    }
    let mut desired = ((cpu / 8.).max(mem / 16.) / 0.8).ceil() as usize;
    desired = desired.clamp(1, e.max_nodes);
    let planned = e.ready + e.starting;
    if e.queued > 0 {
        desired = desired.max((e.ready + 1).min(e.max_nodes));
    }
    let a = if desired >= planned + 2 {
        Action::StartTwo
    } else if desired > planned {
        Action::StartOne
    } else if desired < e.ready
        && e.starting == 0
        && e.queued == 0
        && e.last_change_age_ms.is_none_or(|t| t >= 60_000)
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
#[derive(Clone, Serialize, Deserialize)]
pub struct Inference {
    pub choice: Option<Action>,
    pub confidence: Option<f64>,
    pub probabilities: BTreeMap<String, f64>,
    pub model: Option<String>,
    pub usage: Option<Usage>,
    pub latency_ms: f64,
    pub error: Option<String>,
}
impl Inference {
    pub fn failed(error: impl Into<String>) -> Self {
        Self {
            choice: None,
            confidence: None,
            probabilities: BTreeMap::new(),
            model: None,
            usage: None,
            latency_ms: 0.,
            error: Some(error.into()),
        }
    }
}
pub type Evaluation<'a> = Pin<Box<dyn Future<Output = Inference> + Send + 'a>>;
pub trait Evaluator: Send + Sync {
    fn evaluate(&self, state: Evidence) -> Evaluation<'_>;
}
pub struct LiveEvaluator {
    client: TypeSafeClient,
    model: String,
}
impl LiveEvaluator {
    pub fn new(client: TypeSafeClient, model: String) -> Self {
        Self { client, model }
    }
}
impl Evaluator for LiveEvaluator {
    fn evaluate(&self, state: Evidence) -> Evaluation<'_> {
        Box::pin(async move {
            let start = Instant::now();
            let options = state
                .legal_choices
                .iter()
                .copied()
                .map(|a| (a, a.label()))
                .collect::<Vec<_>>();
            let task=SystemOneTask::builder().model(&self.model).questions(questions! {
                intervention:choice("Choose a capacity action to reduce queue delay and rejections while minimizing active node-seconds. Nodes have 8 CPU and 16 GiB. Starting nodes take startup_s seconds and cannot serve yet; draining nodes finish existing work but take no new jobs. Include starting capacity in your plan. Demand is offered work, not admitted traffic. Forecast p10/p50/p90 are uncertain pointwise quantiles, not guarantees. Compare expected demand during startup and afterwards with ready and pending capacity. Respect the node budget, retain one ready node, avoid oscillation, and hold when no change is useful. Use only supplied evidence. The legal choices are screened again against actual current state before execution",options)
            }).build();
            let task = match task {
                Ok(t) => t,
                Err(e) => return Inference::failed(e.to_string()),
            };
            let judge =
                TypeSafeJudge::new(self.client.clone(), task).select_answer(|a| a.intervention);
            let controller = Controller::builder()
                .judge(judge)
                .inference_timeout(Duration::from_secs(2))
                .build()
                .expect("positive timeout");
            let mut result = match controller.evaluate(&state).await {
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
            result.latency_ms = start.elapsed().as_secs_f64() * 1000.;
            result
        })
    }
}
