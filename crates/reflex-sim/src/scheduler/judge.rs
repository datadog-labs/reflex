// Unless explicitly stated otherwise all files in this repository are licensed under the Apache-2.0 License.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-present Datadog, Inc.

use super::engine::{Choice, Data, Job, JobPhase};
use reflex::Controller;
use reflex_typesafe::TypeSafeJudge;
use serde::Serialize;
use std::{
    collections::BTreeMap,
    future::Future,
    pin::Pin,
    time::{Duration, Instant},
};
use typesafe_ai::{choice, questions, SystemOneTask, TypeSafeClient, Usage};
#[derive(Clone, Serialize)]
pub struct JobEvidence {
    pub priority: super::engine::Priority,
    pub id: u64,
    pub client: u64,
    pub cpu: u32,
    pub memory_gib: u32,
    pub estimated_duration_ms: u64,
    pub waiting_ms: u64,
}
impl JobEvidence {
    fn new(j: &Job, d: &Data) -> Self {
        Self {
            priority: d.priority(j.client),
            id: j.id,
            client: j.client,
            cpu: j.cpu,
            memory_gib: j.memory_gib,
            estimated_duration_ms: j.duration_ms,
            waiting_ms: d.at_ms - j.arrived_at,
        }
    }
}
#[derive(Clone, Serialize)]
pub struct RunningEvidence {
    pub cpu: u32,
    pub memory_gib: u32,
    pub estimated_remaining_ms: u64,
}
#[derive(Clone, Serialize)]
pub struct NodeEvidence {
    pub id: usize,
    pub name: String,
    pub total_cpu: u32,
    pub available_cpu: u32,
    pub total_memory_gib: u32,
    pub available_memory_gib: u32,
    pub running_jobs: Vec<RunningEvidence>,
}
#[derive(Clone)]
pub struct Evidence {
    pub client_performance: Vec<super::engine::ClientStats>,
    pub forecast: Option<crate::forecasting::Evidence>,
    pub telemetry: Option<super::datadog::TelemetryEvidence>,
    pub observed_at_ms: u64,
    pub revision: u64,
    pub candidate: JobEvidence,
    pub candidates: Vec<JobEvidence>,
    pub priority_revision: u64,
    pub aging_ms: u64,
    pub nodes: Vec<NodeEvidence>,
    pub waiting_jobs: usize,
    pub upcoming_jobs: Vec<JobEvidence>,
    pub legal_choices: Vec<Choice>,
}
impl Serialize for Evidence {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut value = serde_json::json!({"observed_at_ms":self.observed_at_ms,"revision":self.revision,
            "client_performance":self.client_performance,"candidate":self.candidate,"nodes":self.nodes,"waiting_jobs":self.waiting_jobs,
            "upcoming_jobs":self.upcoming_jobs,"legal_choices":self.legal_choices,"candidates":self.candidates,"priority_revision":self.priority_revision,"aging_ms":self.aging_ms});
        if let Some(telemetry) = &self.telemetry {
            value["telemetry"] =
                serde_json::to_value(telemetry).map_err(serde::ser::Error::custom)?;
        }
        if let Some(forecast) = &self.forecast {
            value["forecast"] =
                serde_json::to_value(forecast).map_err(serde::ser::Error::custom)?;
        }
        value.serialize(serializer)
    }
}
pub fn evidence(d: &Data) -> Option<Evidence> {
    let candidates = d.candidates();
    let j = *candidates.first()?;
    let mut legal_choices = Vec::new();
    for candidate in &candidates {
        for (node, _) in d
            .nodes
            .iter()
            .enumerate()
            .filter(|(_, n)| n.fits(candidate))
        {
            legal_choices.push(if candidate.id == j.id {
                Choice::for_node(node)
            } else {
                Choice::Place {
                    job: candidate.id,
                    node,
                }
            });
        }
    }
    if d.overdue_head().is_none() {
        legal_choices.push(Choice::Defer);
    }
    Some(Evidence {
        client_performance: d.heads().iter().map(|j| d.client_stats(j.client)).collect(),
        forecast: None,
        telemetry: None,
        observed_at_ms: d.at_ms,
        revision: d.revision,
        candidate: JobEvidence::new(j, d),
        candidates: candidates.iter().map(|j| JobEvidence::new(j, d)).collect(),
        priority_revision: d.priority_revision,
        aging_ms: super::engine::AGING_MS,
        waiting_jobs: d
            .jobs
            .iter()
            .filter(|j| j.phase == JobPhase::Queued)
            .count(),
        upcoming_jobs: d
            .jobs
            .iter()
            .filter(|q| q.phase == JobPhase::Queued && q.id != j.id)
            .take(8)
            .map(|j| JobEvidence::new(j, d))
            .collect(),
        legal_choices,
        nodes: d
            .nodes
            .iter()
            .enumerate()
            .map(|(i, n)| NodeEvidence {
                id: i,
                name: n.name.clone(),
                total_cpu: n.cpu,
                available_cpu: n.cpu - n.used_cpu,
                total_memory_gib: n.memory_gib,
                available_memory_gib: n.memory_gib - n.used_memory_gib,
                running_jobs: d
                    .jobs
                    .iter()
                    .filter(|j| j.phase == JobPhase::Running && j.node == Some(i))
                    .map(|j| RunningEvidence {
                        cpu: j.cpu,
                        memory_gib: j.memory_gib,
                        estimated_remaining_ms: (j.started_at.unwrap() + j.duration_ms)
                            .saturating_sub(d.at_ms),
                    })
                    .collect(),
            })
            .collect(),
    })
}
#[derive(Clone, Serialize)]
pub struct Inference {
    pub choice: Option<Choice>,
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
    fn evidence_source(&self) -> Option<std::sync::Arc<crate::datadog::Source>> {
        None
    }
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
            let options: Vec<_> = state
                .legal_choices
                .iter()
                .map(|c| {
                    let label = match c.node() {
                        Some(n) => format!(
                            "Place job #{} on Node {}",
                            c.job(state.candidate.id),
                            (b'A' + n as u8) as char
                        ),
                        None => "Defer placement; keep all requests queued".into(),
                    };
                    (*c, label)
                })
                .collect();
            let task = SystemOneTask::builder().model(&self.model).questions(questions! {
                placement: choice("Choose a request and node from legal_choices. Each candidate is the oldest queued request from one client; preserve FIFO within each client. Prefer Critical over High over Normal, balancing waiting time and useful throughput. Priorities are live operator preferences, not estimates. When the oldest feasible head has waited 30 seconds, the deterministic aging rule restricts choices to that job and excludes Defer. Node-only choices refer to candidate.id; place choices contain an explicit job ID and node index. Jobs are non-preemptive. Consider CPU/memory fragmentation, estimated durations and upcoming queued work. Defer delays all placement until the next evaluation; use sparingly. Optional forecasts are uncertain demand/pressure projections, not future jobs or permission to exceed capacity. Current jobs, priorities, and node reservations are the placement state. Optional Datadog telemetry is delayed context; missing telemetry must not cause deferral. Never infer current capacity or exact future completion times from delayed metrics. Only choose a supplied legal choice.", options)
            }).build();
            let task = match task {
                Ok(t) => t,
                Err(e) => return Inference::failed(e.to_string()),
            };
            let judge =
                TypeSafeJudge::new(self.client.clone(), task).select_answer(|a| a.placement);
            let controller = Controller::builder()
                .name("resource_scheduler")
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
