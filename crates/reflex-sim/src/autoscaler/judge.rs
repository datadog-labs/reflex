// Unless explicitly stated otherwise all files in this repository are licensed under the Apache-2.0 License.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-present Datadog, Inc.

use super::engine::{
    coverage, legal_choices, Choice, Data, Group, NodePhase, Phase, Resources, HEADROOM_CAP,
    PROVISION_MS, SCALE_DOWN_COOLDOWN_MS,
};
use crate::provider::{choice_judge, ModelClient};
use reflex::{Controller, EvaluationError};
use serde::Serialize;
use std::{
    collections::BTreeMap,
    future::Future,
    pin::Pin,
    time::{Duration, Instant},
};
use typesafe_ai::Usage;

/// How far ahead a forecast may justify provisioning: one node start plus a margin.
pub const HEADROOM_LOOKAHEAD_S: u64 = 60;
/// A forecast built from the simulator's own samples is used for 30 simulated seconds.
pub const LOCAL_FORECAST_MAX_AGE_MS: u64 = 30_000;
/// A forecast built from Datadog history is used until the last bucket it saw is 60 seconds
/// old. Past that, less than the lookahead remains of its 120-second horizon.
pub const DATADOG_FORECAST_MAX_AGE_MS: u64 = 60_000;
pub const DATADOG_FORECAST_SOURCE: &str = "datadog_observations";
/// How old a forecast from this source may be and still justify a scale-up.
pub fn forecast_max_age_ms(source: &str) -> u64 {
    if source == DATADOG_FORECAST_SOURCE {
        DATADOG_FORECAST_MAX_AGE_MS
    } else {
        LOCAL_FORECAST_MAX_AGE_MS
    }
}
/// Where the forecast behind `forecast_headroom` came from and how long it stays usable.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ForecastBasis {
    pub source: String,
    pub time_domain: String,
    pub history_seconds: u64,
    pub age_seconds: u64,
    pub max_age_seconds: u64,
    pub lookahead_seconds: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct PendingEvidence {
    pub workload: &'static str,
    pub count: usize,
    pub cpu: u32,
    pub memory_gib: u32,
    pub oldest_wait_seconds: u64,
    /// Groups whose node shape can hold one of these pods at all.
    pub fits_groups: Vec<Group>,
}
#[derive(Clone, Debug, Serialize)]
pub struct WorkloadEvidence {
    pub name: &'static str,
    pub desired_replicas: u32,
    pub available_replicas: u32,
    pub pending_replicas: usize,
    pub pod_cpu: u32,
    pub pod_memory_gib: u32,
    pub min_available: u32,
}
#[derive(Clone, Debug, Serialize)]
pub struct GroupEvidence {
    pub group: Group,
    pub node_cpu: u32,
    pub node_memory_gib: u32,
    pub hourly_usd: f64,
    pub min_nodes: usize,
    pub max_nodes: usize,
    pub ready: usize,
    pub provisioning: usize,
    pub draining: usize,
    pub provisioning_failures: u32,
    pub seconds_since_last_failure: Option<u64>,
}
#[derive(Clone, Debug, Serialize)]
pub struct NodeEvidence {
    pub name: String,
    pub group: Group,
    pub free_cpu: u32,
    pub free_memory_gib: u32,
    pub pod_count: usize,
    pub pods: BTreeMap<&'static str, usize>,
}
#[derive(Clone, Debug, Serialize)]
pub struct ProvisioningEvidence {
    pub name: String,
    pub group: Group,
    pub estimated_seconds_remaining: u64,
}
/// Exactly what Jev sees. It never includes the scenario, the schedule of future load
/// changes, stock levels, or a node's drawn provisioning time.
#[derive(Clone, Debug, Serialize)]
pub struct Evidence {
    pub observed_at_ms: u64,
    pub revision: u64,
    pub phase: Phase,
    pub pending_pods: Vec<PendingEvidence>,
    /// Pending pods that would still fit nowhere once every provisioning node is ready.
    pub pending_pods_not_covered: usize,
    /// Room for new pods on ready and provisioning nodes after the pending pods are placed.
    pub spare_capacity: Resources,
    pub cpu_utilization_percent: u32,
    pub memory_utilization_percent: u32,
    /// Ready nodes with no pods.
    pub idle_nodes: Vec<String>,
    pub workloads: Vec<WorkloadEvidence>,
    pub node_groups: Vec<GroupEvidence>,
    pub nodes: Vec<NodeEvidence>,
    pub provisioning: Vec<ProvisioningEvidence>,
    pub draining: Vec<String>,
    pub nodes_in_use: usize,
    pub node_budget: usize,
    pub requested: Resources,
    pub ready_capacity: Resources,
    pub seconds_since_last_change: Option<u64>,
    pub seconds_since_scale_up_finished: Option<u64>,
    pub scale_down_cooldown_seconds: u64,
    pub scale_down_cooldown_remaining_seconds: u64,
    pub legal_actions: Vec<Choice>,
    /// Delayed observations queried back from Datadog; context only, never the cluster state.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub telemetry: Option<super::datadog::TelemetryEvidence>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub forecast: Option<crate::forecasting::Evidence>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub forecast_basis: Option<ForecastBasis>,
    /// Simulated time after which this forecast no longer justifies a scale-up.
    #[serde(skip)]
    pub headroom_expires_at_ms: u64,
    /// Extra demand the forecast expects soon; the most a scale-up may claim as justification.
    /// Present only when the forecast expects a rise.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub forecast_headroom: Option<Resources>,
    /// The part of `forecast_headroom` that spare capacity does not already cover.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub forecast_shortfall: Option<Resources>,
    /// Recommendations Reflex refused in the last minute, newest first.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub recent_rejections: Vec<RejectionEvidence>,
}
#[derive(Clone, Debug, Serialize)]
pub struct RejectionEvidence {
    pub action: Choice,
    pub guard: String,
    pub seconds_ago: u64,
}
impl Evidence {
    pub fn headroom(&self) -> Resources {
        self.forecast_headroom.unwrap_or_default()
    }
    /// Attach a forecast and what it implies for capacity. One that is already older than its
    /// source allows is dropped: it is neither shown to Jev nor allowed to justify anything.
    pub fn with_forecast(mut self, forecast: Option<crate::forecasting::Evidence>) -> Self {
        let forecast = forecast.filter(|f| f.age_ms <= forecast_max_age_ms(&f.source));
        self.forecast_basis = forecast.as_ref().map(|f| ForecastBasis {
            source: f.source.clone(),
            time_domain: f.time_domain.clone(),
            history_seconds: f.history_seconds,
            age_seconds: f.age_ms / 1000,
            max_age_seconds: forecast_max_age_ms(&f.source) / 1000,
            lookahead_seconds: HEADROOM_LOOKAHEAD_S,
        });
        self.headroom_expires_at_ms = forecast.as_ref().map_or(0, |f| {
            self.observed_at_ms + forecast_max_age_ms(&f.source) - f.age_ms
        });
        let headroom = forecast
            .as_ref()
            .map(|f| forecast_headroom(f, self.requested))
            .filter(|h| h.cpu > 0 || h.memory_gib > 0);
        let shortfall = headroom.map(|h| Resources {
            cpu: h.cpu.saturating_sub(self.spare_capacity.cpu),
            memory_gib: h.memory_gib.saturating_sub(self.spare_capacity.memory_gib),
        });
        self.forecast = forecast;
        self.forecast_headroom = headroom;
        self.forecast_shortfall = shortfall.filter(|s| s.cpu > 0 || s.memory_gib > 0);
        self
    }
}
pub fn evidence(phase: Phase, d: &Data) -> Evidence {
    let seconds_since = |at: Option<u64>| at.map(|t| (d.at_ms - t) / 1000);
    let mut pending_pods: Vec<PendingEvidence> = Vec::new();
    for p in d.pending() {
        let workload = d.workloads[p.workload].name;
        let wait = (d.at_ms - p.pending_since.unwrap_or(d.at_ms)) / 1000;
        match pending_pods
            .iter_mut()
            .find(|e| e.workload == workload && e.memory_gib == p.memory_gib)
        {
            Some(e) => {
                e.count += 1;
                e.oldest_wait_seconds = e.oldest_wait_seconds.max(wait);
            }
            None => pending_pods.push(PendingEvidence {
                workload,
                count: 1,
                cpu: p.cpu,
                memory_gib: p.memory_gib,
                oldest_wait_seconds: wait,
                fits_groups: d
                    .groups
                    .iter()
                    .filter(|g| p.cpu <= g.cpu && p.memory_gib <= g.memory_gib)
                    .map(|g| g.group)
                    .collect(),
            }),
        }
    }
    let (pending_pods_not_covered, spare_capacity) = coverage(d);
    let ready = d.ready_capacity();
    let placed = |value: fn(&super::engine::Pod) -> u32| -> u32 {
        d.pods.iter().filter(|p| p.node.is_some()).map(value).sum()
    };
    let percent = |used: u32, total: u32| (used * 100).checked_div(total).unwrap_or(0);
    Evidence {
        observed_at_ms: d.at_ms,
        revision: d.revision,
        phase,
        pending_pods,
        pending_pods_not_covered,
        spare_capacity,
        cpu_utilization_percent: percent(placed(|p| p.cpu), ready.cpu),
        memory_utilization_percent: percent(placed(|p| p.memory_gib), ready.memory_gib),
        idle_nodes: d
            .nodes
            .iter()
            .filter(|n| n.phase == NodePhase::Ready && n.used_cpu == 0)
            .map(|n| n.name.clone())
            .collect(),
        workloads: d
            .workloads
            .iter()
            .enumerate()
            .map(|(i, w)| WorkloadEvidence {
                name: w.name,
                desired_replicas: w.desired(),
                available_replicas: d.available(i),
                pending_replicas: d
                    .pods
                    .iter()
                    .filter(|p| p.workload == i && p.node.is_none())
                    .count(),
                pod_cpu: w.cpu,
                pod_memory_gib: w.pod_memory_gib(),
                min_available: w.min_available(),
            })
            .collect(),
        node_groups: d
            .groups
            .iter()
            .map(|g| GroupEvidence {
                group: g.group,
                node_cpu: g.cpu,
                node_memory_gib: g.memory_gib,
                hourly_usd: g.hourly_usd,
                min_nodes: g.min,
                max_nodes: g.max,
                ready: d.count(Some(g.group), NodePhase::Ready),
                provisioning: d.count(Some(g.group), NodePhase::Provisioning),
                draining: d.count(Some(g.group), NodePhase::Draining),
                provisioning_failures: g.failures,
                seconds_since_last_failure: seconds_since(g.last_failure_at),
            })
            .collect(),
        nodes: d
            .nodes
            .iter()
            .filter(|n| n.phase == NodePhase::Ready)
            .map(|n| {
                let mut pods = BTreeMap::new();
                for p in d.pods.iter().filter(|p| p.node == Some(n.id)) {
                    *pods.entry(d.workloads[p.workload].name).or_default() += 1;
                }
                NodeEvidence {
                    name: n.name.clone(),
                    group: n.group,
                    free_cpu: n.free().cpu,
                    free_memory_gib: n.free().memory_gib,
                    pod_count: pods.values().sum(),
                    pods,
                }
            })
            .collect(),
        provisioning: d
            .nodes
            .iter()
            .filter(|n| n.phase == NodePhase::Provisioning)
            .map(|n| ProvisioningEvidence {
                name: n.name.clone(),
                group: n.group,
                // The nominal estimate, not the node's drawn completion time.
                estimated_seconds_remaining: (n.requested_at + PROVISION_MS)
                    .saturating_sub(d.at_ms)
                    .div_ceil(1000),
            })
            .collect(),
        draining: d
            .nodes
            .iter()
            .filter(|n| n.phase == NodePhase::Draining)
            .map(|n| n.name.clone())
            .collect(),
        nodes_in_use: d.active_nodes(),
        node_budget: d.node_budget,
        requested: d.requested(),
        ready_capacity: d.ready_capacity(),
        seconds_since_last_change: seconds_since(d.last_change_at),
        seconds_since_scale_up_finished: seconds_since(d.last_scale_up_finished_at),
        scale_down_cooldown_seconds: SCALE_DOWN_COOLDOWN_MS / 1000,
        scale_down_cooldown_remaining_seconds: d
            .last_scale_up_finished_at
            .map_or(0, |t| (t + SCALE_DOWN_COOLDOWN_MS).saturating_sub(d.at_ms))
            .div_ceil(1000),
        legal_actions: legal_choices(phase, d),
        telemetry: None,
        forecast: None,
        forecast_basis: None,
        headroom_expires_at_ms: 0,
        forecast_headroom: None,
        forecast_shortfall: None,
        recent_rejections: vec![],
    }
}
/// Demand a forecast expects above current requests within the lookahead, capped. The first
/// two series are requested CPU and memory; the p50 path is used, never the upper band.
pub fn forecast_headroom(
    forecast: &crate::forecasting::Evidence,
    requested: Resources,
) -> Resources {
    let horizon = forecast.age_ms / 1000 + HEADROOM_LOOKAHEAD_S;
    let peak = |series: usize| {
        forecast.series.get(series).map_or(0., |s| {
            s.buckets
                .iter()
                .filter(|b| b.from_seconds <= horizon)
                .map(|b| b.p50)
                .fold(0., f64::max)
        })
    };
    let above =
        |peak: f64, now: u32, cap: u32| ((peak - now as f64).max(0.).round() as u32).min(cap);
    Resources {
        cpu: above(peak(0), requested.cpu, HEADROOM_CAP.cpu),
        memory_gib: above(peak(1), requested.memory_gib, HEADROOM_CAP.memory_gib),
    }
}
/// The options offered to Jev: one per legal action, in the same order.
pub fn options(evidence: &Evidence) -> Vec<(Choice, String)> {
    evidence
        .legal_actions
        .iter()
        .map(|c| {
            let label = match c {
                Choice::NoChange => "Keep the cluster as it is".into(),
                Choice::ScaleUp { group, count } => {
                    let g = &evidence.node_groups[group.index()];
                    format!(
                        "Add {count} {} node{} ({} CPU / {} GiB each, ready in about {}s)",
                        group.key(),
                        if *count == 1 { "" } else { "s" },
                        g.node_cpu,
                        g.node_memory_gib,
                        PROVISION_MS / 1000
                    )
                }
                Choice::Remove { group, node } => format!(
                    "Drain and remove node {}; its pods must fit on other ready nodes",
                    super::engine::node_name(*group, *node)
                ),
            };
            (*c, label)
        })
        .collect()
}
const INSTRUCTIONS: &str = "You autoscale a Kubernetes-style cluster. Choose exactly one of legal_actions by applying these rules in order; act on the first rule that applies. \
1. SCALE UP whenever pending_pods_not_covered is above zero, even while other nodes are still provisioning: those pods fit on no ready or provisioning node. Pending pods fit only the groups in their fits_groups; adding any other group leaves them pending. Among the groups that fit, prefer general_large, then general_small, then memory_heavy, skipping a group that is at max_nodes or has provisioning_failures (it is out of stock). Add two nodes when one node of that group cannot hold all the pods that are not covered. \
2. SCALE UP FOR A FORECAST when forecast_shortfall is present: demand will rise within a minute by more than spare_capacity and a node takes about 30 seconds to start, so add general_large now rather than wait for pods to be pending. Add two when forecast_shortfall is more than 8 CPU or 16 GiB. A forecast is never usable capacity. \
3. NO CHANGE when any of these is true: forecast_headroom is present (demand is about to rise, so keep every node); pods are pending or provisioning is not empty (the provisioning nodes will hold them); scale_down_cooldown_remaining_seconds is above zero. \
4. SCALE DOWN when idle_nodes is not empty, or cpu_utilization_percent and memory_utilization_percent are both below 70: spare nodes cost money, so remove one node now. Remove an idle node first. Otherwise remove the node with the lowest pod_count whose pods all fit into the free_cpu and free_memory_gib of the other ready nodes, keeping min_available replicas of each workload on other nodes. Never choose an action listed in recent_rejections: Reflex refused it and the cluster has not changed since. \
5. Otherwise NO CHANGE. \
telemetry, when present, holds delayed Datadog observations of this cluster and forecast_basis says where the forecast came from; both are context. The fields outside telemetry are the live cluster: decide from them, and never read missing or older telemetry as no demand or no pending pods. \
Reflex re-checks freshness, limits, justification, cooldown, drainability and disruption budgets against the live cluster when the action executes; confidence bypasses none of them.";

#[derive(Clone, Debug, Serialize)]
pub struct Failure {
    pub code: String,
    pub message: String,
}
#[derive(Clone, Debug, Serialize)]
pub struct Inference {
    pub choice: Option<Choice>,
    pub confidence: Option<f64>,
    pub probabilities: BTreeMap<String, f64>,
    pub model: Option<String>,
    pub usage: Option<Usage>,
    pub request_id: Option<String>,
    pub latency_ms: f64,
    pub error: Option<Failure>,
}
impl Inference {
    pub fn failed(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            choice: None,
            confidence: None,
            probabilities: BTreeMap::new(),
            model: None,
            usage: None,
            request_id: None,
            latency_ms: 0.,
            error: Some(Failure {
                code: code.into(),
                message: message.into(),
            }),
        }
    }
    pub fn chose(choice: Choice, confidence: Option<f64>) -> Self {
        Self {
            choice: Some(choice),
            confidence,
            error: None,
            ..Self::failed("", "")
        }
    }
}
pub type Evaluation<'a> = Pin<Box<dyn Future<Output = Inference> + Send + 'a>>;
pub trait Evaluator: Send + Sync {
    /// Present when Jev's evidence should also carry telemetry queried from Datadog.
    fn evidence_source(&self) -> Option<std::sync::Arc<crate::datadog::Source>> {
        None
    }
    fn evaluate(&self, evidence: Evidence) -> Evaluation<'_>;
}
pub struct LiveEvaluator {
    client: ModelClient,
    model: String,
    meter: Option<opentelemetry::metrics::Meter>,
}
impl LiveEvaluator {
    pub fn new(client: impl Into<ModelClient>, model: String) -> Self {
        Self {
            client: client.into(),
            model,
            meter: None,
        }
    }
    /// Count `reflex.evaluations` on an application-owned meter instead of the global one.
    pub fn with_meter(mut self, meter: opentelemetry::metrics::Meter) -> Self {
        self.meter = Some(meter);
        self
    }
}
impl Evaluator for LiveEvaluator {
    fn evaluate(&self, evidence: Evidence) -> Evaluation<'_> {
        Box::pin(async move {
            let start = Instant::now();
            // A fresh adapter per evaluation makes diagnostics belong to this response.
            let judge = choice_judge!(
                &self.client,
                &self.model,
                action: INSTRUCTIONS,
                options(&evidence)
            );
            let judge = match judge {
                Ok(j) => j,
                Err(e) => return Inference::failed("configuration", e),
            };
            let mut controller = Controller::builder().name("cluster_autoscaler");
            if let Some(meter) = &self.meter {
                controller = controller.meter(meter.clone());
            }
            let controller = controller
                .judge(judge)
                .inference_timeout(Duration::from_secs(2))
                .build()
                .expect("positive deadline");
            let mut inference = match controller.evaluate(&evidence).await {
                Ok(proposal) => {
                    let diagnostics = controller.judge().last_response();
                    Inference {
                        choice: Some(*proposal.action()),
                        confidence: proposal.confidence(),
                        probabilities: diagnostics
                            .as_ref()
                            .map(|d| d.probabilities.clone())
                            .unwrap_or_default(),
                        model: diagnostics.as_ref().map(|d| d.model.clone()),
                        usage: diagnostics.as_ref().and_then(|d| d.usage.clone()),
                        request_id: diagnostics.and_then(|d| d.request_id),
                        latency_ms: 0.,
                        error: None,
                    }
                }
                Err(e) => Inference::failed(
                    match &e {
                        EvaluationError::Timeout => "timeout".into(),
                        EvaluationError::Judge(e) => e.code.clone(),
                        EvaluationError::InvalidConfidence => "invalid_confidence".into(),
                    },
                    e.to_string(),
                ),
            };
            inference.latency_ms = start.elapsed().as_secs_f64() * 1000.;
            inference
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::autoscaler::engine::{Action, Control, Engine};
    use crate::forecasting::{Bucket, Prediction};

    #[tokio::test]
    async fn evidence_describes_the_cluster_without_hidden_state() {
        let mut e = Engine::new(42).unwrap();
        let stockout = Control::Stockout {
            group: Group::MemoryHeavy,
            enabled: true,
        };
        e.control(stockout).await.unwrap();
        let heavy = Control::MemoryHeavy {
            workload: 1,
            enabled: true,
        };
        e.control(heavy).await.unwrap();
        let d = e.data();
        let action = Action {
            choice: Choice::ScaleUp {
                group: Group::MemoryHeavy,
                count: 1,
            },
            revision: d.revision,
            observed_at: d.at_ms,
            headroom: Resources::default(),
            headroom_expires_at: 0,
        };
        assert_eq!(e.apply(action, None).await.unwrap().status, "applied");
        e.advance(4_000).await.unwrap();

        let (phase, d) = e.state();
        let evidence = evidence(phase, &d);
        let json = serde_json::to_value(&evidence).unwrap();
        let text = json.to_string();
        for hidden in [
            "seed", "due_at", "doomed", "stockout", "scenario", "schedule",
        ] {
            assert!(!text.contains(hidden), "{hidden}");
        }
        assert_eq!(json["phase"], "scaling_up");
        assert_eq!(json["revision"], d.revision);
        assert_eq!(json["observed_at_ms"], 4_000);
        let pending = &json["pending_pods"][0];
        assert_eq!(pending["workload"], "api");
        assert_eq!(pending["count"], 2);
        assert_eq!(pending["memory_gib"], 24);
        assert_eq!(pending["oldest_wait_seconds"], 4);
        assert_eq!(pending["fits_groups"], serde_json::json!(["memory_heavy"]));
        // The doomed node fails at 10s; Jev still sees only the nominal estimate.
        assert_eq!(json["provisioning"][0]["name"], "mem-5");
        assert_eq!(json["provisioning"][0]["estimated_seconds_remaining"], 26);
        assert_eq!(json["node_groups"][2]["provisioning"], 1);
        assert_eq!(json["node_groups"][2]["provisioning_failures"], 0);
        assert_eq!(json["nodes"].as_array().unwrap().len(), 4);
        assert_eq!(json["nodes"][2]["name"], "large-3");
        assert_eq!(json["nodes"][2]["free_cpu"], 4);
        assert_eq!(json["nodes"][2]["pods"]["web"], 2);
        assert_eq!(json["seconds_since_last_change"], 4);
        assert_eq!(json["scale_down_cooldown_remaining_seconds"], 0);
        assert_eq!(
            json["seconds_since_scale_up_finished"],
            serde_json::Value::Null
        );
        assert_eq!(json["nodes_in_use"], 5);
        // Derived signals: the provisioning node will hold both pending pods.
        assert_eq!(json["pending_pods_not_covered"], 0);
        assert_eq!(json["cpu_utilization_percent"], 50);
        assert_eq!(json["idle_nodes"], serde_json::json!([]));
        assert!(json.get("recent_rejections").is_none());
        assert!(json.get("forecast").is_none() && json.get("forecast_headroom").is_none());

        // Jev is offered exactly the legal actions, under the labels the evidence lists.
        let legal = legal_choices(phase, &d);
        let offered = options(&evidence);
        assert_eq!(evidence.legal_actions, legal);
        assert_eq!(offered.iter().map(|(c, _)| *c).collect::<Vec<_>>(), legal);
        assert_eq!(
            json["legal_actions"],
            serde_json::json!(legal.iter().map(Choice::label).collect::<Vec<_>>())
        );
        assert_eq!(json["legal_actions"][0], "no_change");
        assert!(legal.contains(&Choice::ScaleUp {
            group: Group::GeneralLarge,
            count: 2
        }));
        assert!(!legal.iter().any(|c| matches!(c, Choice::Remove { .. })));
        e.advance(40_000).await.unwrap();
        let (phase, d) = e.state();
        let evidence = super::evidence(phase, &d);
        assert_eq!(evidence.node_groups[2].provisioning_failures, 1);
        assert_eq!(evidence.node_groups[2].seconds_since_last_failure, Some(30));
        let labels: Vec<_> = evidence.legal_actions.iter().map(Choice::label).collect();
        assert!(labels.contains(&"remove:small-1".to_owned()));
        assert!(labels.contains(&"scale_up:memory_heavy:1".to_owned()));
        assert_eq!(options(&evidence).len(), labels.len());
    }

    #[test]
    fn a_forecast_is_dropped_once_it_is_older_than_its_source_allows() {
        let forecast = |source: &str, age_ms: u64| {
            let series = |name: &str, p50: f64| Prediction {
                name: name.into(),
                buckets: vec![Bucket {
                    from_seconds: age_ms / 1000 + 1,
                    through_seconds: age_ms / 1000 + 10,
                    p10: 0.,
                    p50,
                    p90: 500.,
                }],
            };
            let remote = source == DATADOG_FORECAST_SOURCE;
            crate::forecasting::Evidence {
                source: source.into(),
                time_domain: if remote {
                    "forecast_epoch_wall_clock"
                } else {
                    "simulation"
                }
                .into(),
                origin_ms: 0,
                age_ms,
                history_seconds: if remote { 320 } else { 180 },
                horizon_seconds: 120,
                model_provenance: "test".into(),
                series: vec![
                    series("requested_cpu", 30.),
                    series("requested_memory_gib", 60.),
                    series("pending_pods", 0.),
                ],
            }
        };
        let d = Engine::new(42).unwrap().data();
        let mut base = evidence(Phase::Stable, &d);
        base.observed_at_ms = 100_000;

        // Datadog history is already delayed; its forecast is used until it is 60 seconds old.
        let fresh = base
            .clone()
            .with_forecast(Some(forecast(DATADOG_FORECAST_SOURCE, 59_000)));
        let basis = fresh.forecast_basis.clone().unwrap();
        assert_eq!(basis.source, "datadog_observations");
        assert_eq!(basis.time_domain, "forecast_epoch_wall_clock");
        assert_eq!((basis.age_seconds, basis.max_age_seconds), (59, 60));
        assert_eq!((basis.history_seconds, basis.lookahead_seconds), (320, 60));
        assert_eq!(fresh.headroom().cpu, 12);
        assert_eq!(fresh.headroom_expires_at_ms, 101_000);
        let json = serde_json::to_value(&fresh).unwrap();
        assert_eq!(json["forecast_basis"]["source"], "datadog_observations");
        assert_eq!(json["forecast_headroom"]["memory_gib"], 20);
        assert!(json.get("headroom_expires_at_ms").is_none());
        let stale = base
            .clone()
            .with_forecast(Some(forecast(DATADOG_FORECAST_SOURCE, 60_001)));
        assert!(stale.forecast.is_none() && stale.forecast_basis.is_none());
        assert!(stale.forecast_headroom.is_none() && stale.forecast_shortfall.is_none());
        assert_eq!(stale.headroom_expires_at_ms, 0);
        let json = serde_json::to_value(&stale).unwrap();
        assert!(json.get("forecast").is_none() && json.get("forecast_basis").is_none());

        // The simulator's own samples are current, so their forecast is used for 30 seconds.
        let local = base
            .clone()
            .with_forecast(Some(forecast("simulator_observations", 30_000)));
        let basis = local.forecast_basis.clone().unwrap();
        assert_eq!(
            (basis.source.as_str(), basis.max_age_seconds),
            ("simulator_observations", 30)
        );
        assert_eq!(local.headroom_expires_at_ms, 100_000);
        let stale = base.with_forecast(Some(forecast("simulator_observations", 30_001)));
        assert!(stale.forecast.is_none() && stale.forecast_headroom.is_none());
        assert_eq!(
            forecast_max_age_ms("anything else"),
            LOCAL_FORECAST_MAX_AGE_MS
        );
    }

    #[test]
    fn forecast_headroom_is_the_capped_p50_rise_within_the_lookahead() {
        let series = |name: &str, values: &[(u64, f64)]| Prediction {
            name: name.into(),
            buckets: values
                .iter()
                .map(|(from, p50)| Bucket {
                    from_seconds: *from,
                    through_seconds: from + 9,
                    p10: 0.,
                    p50: *p50,
                    p90: 500.,
                })
                .collect(),
        };
        let mut forecast = crate::forecasting::Evidence {
            source: "simulator_observations".into(),
            time_domain: "simulation".into(),
            origin_ms: 100_000,
            age_ms: 10_000,
            history_seconds: 100,
            horizon_seconds: 120,
            model_provenance: "test".into(),
            series: vec![
                series("requested_cpu", &[(11, 20.), (51, 27.6), (71, 90.)]),
                series("requested_memory_gib", &[(11, 30.), (51, 39.), (71, 400.)]),
                series("pending_pods", &[(11, 9.)]),
            ],
        };
        let requested = Resources {
            cpu: 18,
            memory_gib: 40,
        };
        // Buckets starting beyond the lookahead are ignored, as is the p90 band.
        let headroom = forecast_headroom(&forecast, requested);
        assert_eq!((headroom.cpu, headroom.memory_gib), (10, 0));
        forecast.age_ms = 30_000;
        let headroom = forecast_headroom(&forecast, requested);
        assert_eq!(
            (headroom.cpu, headroom.memory_gib),
            (HEADROOM_CAP.cpu, HEADROOM_CAP.memory_gib)
        );

        // Jev is told about a rise only when there is one, and how much of it is uncovered.
        let d = Engine::new(42).unwrap().data();
        let rising = evidence(Phase::Stable, &d).with_forecast(Some(forecast.clone()));
        assert_eq!(rising.forecast_headroom, Some(HEADROOM_CAP));
        assert_eq!(rising.forecast_shortfall, Some(HEADROOM_CAP));
        assert_eq!(rising.headroom(), HEADROOM_CAP);
        let mut covered = evidence(Phase::Stable, &d);
        covered.spare_capacity = HEADROOM_CAP;
        let covered = covered.with_forecast(Some(forecast.clone()));
        assert!(covered.forecast_headroom.is_some() && covered.forecast_shortfall.is_none());
        for s in &mut forecast.series {
            s.buckets.iter_mut().for_each(|b| b.p50 = 1.);
        }
        let flat = evidence(Phase::Stable, &d).with_forecast(Some(forecast));
        assert!(flat.forecast.is_some());
        assert!(flat.forecast_headroom.is_none() && flat.forecast_shortfall.is_none());
        assert_eq!(flat.headroom(), Resources::default());
    }
}
