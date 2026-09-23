// Unless explicitly stated otherwise all files in this repository are licensed under the Apache-2.0 License.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-present Datadog, Inc.

use super::engine::{self, Action, Data, Lifecycle};
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
pub struct Window {
    pub responses: usize,
    pub success_rate: f64,
    pub essential_success_rate: f64,
    pub p95_ms: u64,
    pub rejected: usize,
}
fn window(d: &Data, ms: u64) -> Window {
    let rows: Vec<_> = d
        .outcomes
        .iter()
        .filter(|r| r.at_ms + ms >= d.at_ms)
        .collect();
    let mut latency: Vec<_> = rows
        .iter()
        .filter(|r| r.success)
        .map(|r| r.latency_ms)
        .collect();
    latency.sort();
    let essential = rows.iter().filter(|r| r.essential).count();
    Window {
        responses: rows.len(),
        success_rate: rows.iter().filter(|r| r.success).count() as f64 / rows.len().max(1) as f64,
        essential_success_rate: rows.iter().filter(|r| r.essential && r.success).count() as f64
            / essential.max(1) as f64,
        p95_ms: if latency.is_empty() {
            0
        } else {
            latency[(latency.len() * 95 / 100).min(latency.len() - 1)]
        },
        rejected: rows.iter().filter(|r| r.rejected).count(),
    }
}
#[derive(Clone, Serialize)]
pub struct ReplicaEvidence {
    pub id: usize,
    pub name: String,
    pub phase: Lifecycle,
    pub serving: bool,
    pub snapshot_version: u64,
    pub reachable: bool,
    pub transfer_reachable: bool,
    pub heartbeat_age_ms: u64,
    pub in_flight: usize,
    pub queue_depth: usize,
    pub responses_5s: usize,
    pub success_rate_5s: f64,
    pub p95_ms_5s: u64,
}
#[derive(Clone)]
pub struct Evidence {
    pub forecast: Option<crate::forecasting::Evidence>,
    pub telemetry: Option<super::datadog::TelemetryEvidence>,
    pub observed_at_ms: u64,
    pub revision: u64,
    pub desired_replicas: usize,
    pub required_snapshot_version: u64,
    pub arrival_rate: f64,
    pub essential_fraction: f64,
    pub short_window: Window,
    pub long_window: Window,
    pub replicas: Vec<ReplicaEvidence>,
    pub essential_only: bool,
    pub retries_enabled: bool,
    pub retry_credit: f64,
    pub retry_limit: String,
    pub bandwidth_limit_mb_s: f64,
    pub recovery: Option<engine::Recovery>,
    pub intervention_requested: bool,
    pub last_action_age_ms: u64,
    pub recent_actions: Vec<engine::ActionRecord>,
    pub legal_choices: Vec<Action>,
}
impl Serialize for Evidence {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut value = if let Some(t) = &self.telemetry {
            let replicas:Vec<_>=self.replicas.iter().map(|r|serde_json::json!({
                "id":r.id,"name":r.name,"phase":r.phase,"serving":r.serving,"snapshot_version":r.snapshot_version
            })).collect();
            let recovery=self.recovery.as_ref().map(|r|serde_json::json!({
                "id":r.id,"source":r.source,"target":r.target,"phase":r.phase,"configured_rate_mb_s":r.rate
            }));
            serde_json::json!({"control":{
                "observed_at_ms":self.observed_at_ms,"revision":self.revision,
                "desired_replicas":self.desired_replicas,"required_snapshot_version":self.required_snapshot_version,
                "configured_arrival_rate":self.arrival_rate,"configured_essential_fraction":self.essential_fraction,
                "replicas":replicas,"essential_only":self.essential_only,"retries_enabled":self.retries_enabled,
                "retry_credit":self.retry_credit,"retry_limit":self.retry_limit,"bandwidth_limit_mb_s":self.bandwidth_limit_mb_s,
                "recovery":recovery,"intervention_requested":self.intervention_requested,
                "last_action_age_ms":self.last_action_age_ms,"recent_actions":self.recent_actions,"legal_choices":self.legal_choices
            },"telemetry":t})
        } else {
            serde_json::json!({"observed_at_ms":self.observed_at_ms,
"revision":self.revision,
"desired_replicas":self.desired_replicas,
"required_snapshot_version":self.required_snapshot_version,
"arrival_rate":self.arrival_rate,
"essential_fraction":self.essential_fraction,
"short_window":self.short_window,
"long_window":self.long_window,
"replicas":self.replicas,
"essential_only":self.essential_only,
"retries_enabled":self.retries_enabled,
"retry_credit":self.retry_credit,
"retry_limit":self.retry_limit,
"bandwidth_limit_mb_s":self.bandwidth_limit_mb_s,
"recovery":self.recovery,
"intervention_requested":self.intervention_requested,
"last_action_age_ms":self.last_action_age_ms,
"recent_actions":self.recent_actions,
"legal_choices":self.legal_choices})
        };
        if let Some(forecast) = &self.forecast {
            value["forecast"] =
                serde_json::to_value(forecast).map_err(serde::ser::Error::custom)?;
        }
        value.serialize(serializer)
    }
}
pub fn evidence(d: &Data) -> Evidence {
    let demand: f64 = d
        .clients
        .iter()
        .filter(|c| c.config.enabled)
        .map(|c| c.config.rate)
        .sum();
    Evidence {
        forecast: None,
        telemetry: None,
        observed_at_ms: d.at_ms,
        revision: d.revision,
        desired_replicas: 3,
        required_snapshot_version: 1,
        arrival_rate: demand,
        essential_fraction: d
            .clients
            .iter()
            .filter(|c| c.config.enabled)
            .map(|c| c.config.rate * c.config.essential_pct as f64 / 100.)
            .sum::<f64>()
            / demand.max(0.001),
        short_window: window(d, 1000),
        long_window: window(d, 5000),
        replicas: d
            .replicas
            .iter()
            .map(|n| {
                let rows: Vec<_> = d
                    .outcomes
                    .iter()
                    .filter(|r| r.node == Some(n.id) && r.at_ms + 5000 >= d.at_ms)
                    .collect();
                let mut lat: Vec<_> = rows
                    .iter()
                    .filter(|r| r.success)
                    .map(|r| r.latency_ms)
                    .collect();
                lat.sort();
                let load = d.requests.iter().filter(|r| r.node == n.id).count();
                ReplicaEvidence {
                    id: n.id,
                    name: n.name.clone(),
                    phase: n.phase,
                    serving: n.serving,
                    snapshot_version: n.version,
                    reachable: n.reachable,
                    transfer_reachable: n.transfer_reachable,
                    heartbeat_age_ms: d.at_ms - n.heartbeat_at,
                    in_flight: load,
                    queue_depth: load.saturating_sub(4),
                    responses_5s: rows.len(),
                    success_rate_5s: rows.iter().filter(|r| r.success).count() as f64
                        / rows.len().max(1) as f64,
                    p95_ms_5s: if lat.is_empty() {
                        0
                    } else {
                        lat[lat.len() * 95 / 100]
                    },
                }
            })
            .collect(),
        essential_only: d.essential_only,
        retries_enabled: d.retries_enabled,
        retry_credit: d.retry_credit,
        retry_limit: "One retry per request; one credit per ten arrivals, bucket capacity two"
            .into(),
        bandwidth_limit_mb_s: d.bandwidth_limit,
        recovery: d.recovery.clone(),
        intervention_requested: d.intervention,
        last_action_age_ms: d.at_ms - d.last_action,
        recent_actions: d.recent_actions.clone(),
        legal_choices: engine::actions(d),
    }
}
pub fn baseline(e: &Evidence) -> Action {
    let allowed = |a| e.legal_choices.contains(&a);
    for n in &e.replicas {
        let a = Action::RemoveFromServing { replica: n.id };
        if n.serving
            && (n.phase != Lifecycle::Ready || (n.responses_5s >= 8 && n.success_rate_5s < 0.5))
            && allowed(a)
        {
            return a;
        }
    }
    if e.replicas
        .iter()
        .filter(|n| n.serving && n.phase == Lifecycle::Ready)
        .count()
        < 3
    {
        for n in &e.replicas {
            let a = Action::AddToServing { replica: n.id };
            if allowed(a) {
                return a;
            }
        }
    }
    let distressed = e.long_window.responses >= 10
        && (e.long_window.essential_success_rate < 0.8
            || e.replicas.iter().any(|n| n.serving && n.queue_depth > 8));
    if let Some(r) = e.recovery.as_ref().filter(|r| r.phase == "rebuilding") {
        let a = Action::SetRebuildRate { high: !distressed };
        if (r.rate > 4. && distressed || r.rate < 12. && !distressed) && allowed(a) {
            return a;
        }
    }
    if !e.essential_only && distressed {
        return Action::SetServingMode {
            essential_only: true,
        };
    }
    let mut sources: Vec<_> = e.replicas.iter().collect();
    sources.sort_by_key(|n| n.in_flight);
    let mut targets: Vec<_> = e.replicas.iter().collect();
    targets.sort_by_key(|n| if n.phase == Lifecycle::Empty { 0 } else { 1 });
    for target in targets {
        for source in &sources {
            let a = Action::StartRebuild {
                source: source.id,
                target: target.id,
            };
            if allowed(a) {
                return a;
            }
        }
    }
    if e.essential_only
        && !distressed
        && e.last_action_age_ms >= 5000
        && e.replicas
            .iter()
            .filter(|n| n.serving && n.phase == Lifecycle::Ready)
            .count()
            >= 3
    {
        return Action::SetServingMode {
            essential_only: false,
        };
    }
    if !e.replicas.iter().any(|n| n.phase == Lifecycle::Ready)
        && allowed(Action::RequestIntervention)
    {
        return Action::RequestIntervention;
    }
    Action::KeepCurrentPlan
}
#[derive(Clone, Serialize)]
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
            let options = state
                .legal_choices
                .iter()
                .copied()
                .map(|a| (a, a.label()))
                .collect::<Vec<_>>();
            let task=SystemOneTask::builder().model(&self.model).questions(questions! {
                intervention:choice("Protect successful essential reads while restoring three ready replicas. Optional Toto forecasts describe continuation of observed load and pressure with uncertainty; use them to anticipate serving and rebuild contention. They do not predict injected faults or establish replica readiness. Forecasts never override legal actions, retry limits, or resource bounds. Choose one legal action. In Datadog mode, control is current authoritative configuration and legal choices; telemetry contains delayed 30-to-120-second observations with timestamps. Null statistics mean unknown, not healthy. Per-replica client results include final failures that never reached a server. Rebuild throughput is historical, not current progress. Use current control to determine action eligibility and telemetry to assess health. Use only observations, never assume a fault duration or hidden cause. Exclude unhealthy serving replicas, include verified replacements, and restore redundancy with a rebuild when useful. Rebuilding competes with reads for CPU; high rate is faster but more disruptive. Prefer low rate under load. Retries consume a fixed global budget and add load. Essential-only mode rejects optional traffic; restore normal mode when safe. Keep the current plan when no change is useful; request intervention if no safe automated recovery exists. Progress and response statistics are observations, not guarantees. Avoid oscillation.",options)
            }).build();
            let task = match task {
                Ok(t) => t,
                Err(e) => return Inference::failed(e.to_string()),
            };
            let judge =
                TypeSafeJudge::new(self.client.clone(), task).select_answer(|a| a.intervention);
            let controller = Controller::builder()
                .name("retry_recovery")
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
