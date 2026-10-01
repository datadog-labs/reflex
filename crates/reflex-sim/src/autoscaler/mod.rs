// Unless explicitly stated otherwise all files in this repository are licensed under the Apache-2.0 License.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-present Datadog, Inc.

//! Cluster autoscaling: Jev recommends node changes; one guarded machine owns the cluster.
pub mod datadog;
pub mod engine;
pub mod judge;
pub mod presets;
mod telemetry;
pub mod web;
use crate::decision_trace::DecisionTrace;
use crate::{
    playground::inference::{CostStatus, JevSettings},
    Error,
};
use engine::{
    Action, Choice, Control, Data, Engine, Node, NodeGroup, NodePhase, Outcome, Phase, Pod,
    Resources, Transition, Workload,
};
use judge::{Evaluator, Evidence, Inference};
use opentelemetry::metrics::Meter;
use presets::Change;
pub use presets::Scenario;
use serde::{Deserialize, Serialize};
use std::{
    sync::{Arc, Mutex},
    time::Instant,
};
use tokio::task::JoinHandle;
use tracing::Instrument;

/// Simulated time between Jev evaluations.
pub const EVALUATION_INTERVAL_MS: u64 = 5_000;
/// How long a refused recommendation stays in Jev's evidence.
pub const REJECTION_MEMORY_MS: u64 = 60_000;
/// Wall-clock time between Datadog evidence queries.
const FETCH_INTERVAL: std::time::Duration = std::time::Duration::from_secs(10);
/// After play or resume, only observations this much later than the boundary are used.
const RESUME_MARGIN_MS: u64 = 20_000;
const SERIES: [&str; 3] = ["requested_cpu", "requested_memory_gib", "pending_pods"];
const COLLECTING: &str = "Deciding from the current cluster; collecting Datadog context";

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Command {
    Scenario { scenario: Scenario },
    Forecast { enabled: bool },
    Play,
    Pause,
    Step,
    Speed { value: u8 },
    Control { control: Control },
    Reset,
}
#[derive(Clone, Serialize)]
pub struct Decision {
    pub id: u64,
    pub at_ms: u64,
    pub observed_at_ms: u64,
    pub choice: Option<Choice>,
    pub status: &'static str,
    pub code: Option<String>,
    pub reason: String,
    pub from: Phase,
    pub to: Phase,
    /// The exact state sent to Jev and its reply.
    pub evidence: Evidence,
    pub result: Inference,
}
#[derive(Clone, Serialize)]
pub struct ControlChange {
    pub at_ms: u64,
    pub control: Control,
    pub source: &'static str,
}
#[derive(Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TimelineEvent {
    Transition(Transition),
    Control(ControlChange),
    Demand {
        at_ms: u64,
        workload: usize,
        replicas: u32,
    },
    Decision(Box<Decision>),
}
impl TimelineEvent {
    pub fn at_ms(&self) -> u64 {
        match self {
            Self::Transition(t) => t.at_ms,
            Self::Control(c) => c.at_ms,
            Self::Demand { at_ms, .. } => *at_ms,
            Self::Decision(d) => d.at_ms,
        }
    }
}
#[derive(Clone, Serialize)]
pub struct Sample {
    pub at_ms: u64,
    pub requested_cpu: u32,
    pub requested_memory_gib: u32,
    pub ready_cpu: u32,
    pub ready_memory_gib: u32,
    pub pending_pods: usize,
    pub nodes: usize,
    pub provisioning: usize,
}
#[derive(Serialize)]
pub struct WorkloadView {
    #[serde(flatten)]
    pub workload: Workload,
    pub desired: u32,
    pub pod_memory_gib: u32,
    pub available: u32,
    pub pending: usize,
    pub min_available: u32,
}
#[derive(Serialize)]
pub struct GroupView {
    #[serde(flatten)]
    pub group: NodeGroup,
    pub ready: usize,
    pub provisioning: usize,
    pub draining: usize,
}
#[derive(Serialize)]
pub struct PendingStatus {
    pub observed_at_ms: u64,
    pub response_ready: bool,
}
#[derive(Serialize)]
pub struct View {
    pub scenario: Scenario,
    pub scenario_description: String,
    pub forecast: crate::forecasting::View,
    pub forecast_headroom: Option<Resources>,
    pub at_ms: u64,
    pub horizon_ms: u64,
    pub seed: u64,
    pub paused: bool,
    pub speed: u8,
    pub available: bool,
    pub model: String,
    pub phase: Phase,
    pub revision: u64,
    pub workloads: Vec<WorkloadView>,
    pub groups: Vec<GroupView>,
    pub nodes: Vec<Node>,
    pub pods: Vec<Pod>,
    pub node_budget: usize,
    pub active_nodes: usize,
    pub pending_pods: usize,
    pub oldest_pending_ms: u64,
    pub pending_pod_seconds: f64,
    pub requested: Resources,
    pub ready_capacity: Resources,
    pub hourly_usd: f64,
    pub cluster_cost_usd: f64,
    pub provision_ms: u64,
    pub scale_down_cooldown_ms: u64,
    pub legal_actions: Vec<Choice>,
    pub history: Vec<Sample>,
    pub transitions: Vec<Transition>,
    pub controls: Vec<ControlChange>,
    pub decisions: Vec<Decision>,
    pub pending: Option<PendingStatus>,
    pub calls: usize,
    pub cost: CostStatus,
    pub status: String,
    /// `datadog` when Jev's evidence also carries telemetry queried from Datadog.
    pub evidence_source: &'static str,
    /// Tags every metric this run publishes.
    pub simulation_run: String,
    pub telemetry_status: Option<String>,
    /// The latest usable Datadog observations, with their current age.
    pub telemetry: Option<datadog::TelemetryEvidence>,
    pub error: Option<String>,
}
struct Pending {
    id: u64,
    evidence: Evidence,
    task: JoinHandle<Inference>,
    trace: DecisionTrace,
}
impl Drop for Pending {
    fn drop(&mut self) {
        self.task.abort();
    }
}
struct Fetch {
    task: JoinHandle<Result<datadog::TelemetryEvidence, String>>,
}
impl Drop for Fetch {
    fn drop(&mut self) {
        self.task.abort();
    }
}
pub struct Session {
    scenario: Scenario,
    script: Vec<(u64, Change)>,
    script_cursor: usize,
    pub(crate) forecast: crate::forecasting::Driver,
    source: Option<Arc<crate::datadog::Source>>,
    run: String,
    not_before: u64,
    fetch: Option<Fetch>,
    last_fetch: Option<Instant>,
    cached: Option<datadog::TelemetryEvidence>,
    telemetry_status: String,
    engine: Engine,
    meter: Meter,
    telemetry: telemetry::Telemetry,
    seed: u64,
    paused: bool,
    speed: u8,
    evaluator: Option<Arc<dyn Evaluator>>,
    settings: JevSettings,
    pending: Option<Pending>,
    last_dispatch: Option<Instant>,
    next_due_ms: u64,
    calls: usize,
    cost: Arc<Mutex<CostStatus>>,
    decisions: Vec<Decision>,
    timeline: Vec<TimelineEvent>,
    history: Vec<Sample>,
    pub error: Option<String>,
}
impl Session {
    pub fn new(
        seed: u64,
        evaluator: Option<Arc<dyn Evaluator>>,
        settings: JevSettings,
    ) -> Result<Self, Error> {
        Self::with_meter(
            seed,
            evaluator,
            settings,
            opentelemetry::global::meter("autoscaler"),
        )
    }
    /// Publish this session's metrics, and its machine's `reflex.transitions`, on an
    /// application-owned meter. `new` uses the global provider, which exports nothing
    /// unless the application installed an exporter.
    pub fn with_meter(
        seed: u64,
        evaluator: Option<Arc<dyn Evaluator>>,
        settings: JevSettings,
        meter: Meter,
    ) -> Result<Self, Error> {
        if settings.model.trim().is_empty() {
            return Err(Error::Invalid("Jev requires a model".into()));
        }
        let source = evaluator.as_ref().and_then(|e| e.evidence_source());
        let run = datadog::new_run();
        let mut session = Self {
            scenario: Scenario::Live,
            script: vec![],
            script_cursor: 0,
            forecast: Default::default(),
            source,
            not_before: crate::datadog::unix_ms(),
            fetch: None,
            last_fetch: None,
            cached: None,
            telemetry_status: COLLECTING.into(),
            engine: Engine::with_meter(seed, meter.clone())?,
            telemetry: telemetry::Telemetry::new(&meter, &run),
            run,
            meter,
            seed,
            paused: true,
            speed: 1,
            evaluator,
            settings,
            pending: None,
            last_dispatch: None,
            next_due_ms: 0,
            calls: 0,
            cost: Arc::new(Mutex::new(CostStatus::default())),
            decisions: vec![],
            timeline: vec![],
            history: vec![],
            error: None,
        };
        session.refresh_telemetry();
        Ok(session)
    }
    pub fn data(&self) -> Data {
        self.engine.data()
    }
    pub fn phase(&self) -> Phase {
        self.engine.phase()
    }
    pub fn decisions(&self) -> &[Decision] {
        &self.decisions
    }
    /// Transitions, load and control changes, and Jev decisions, in the order they happened.
    pub fn timeline(&self) -> &[TimelineEvent] {
        &self.timeline
    }
    /// True when Jev's evidence also carries telemetry and forecasts queried from Datadog.
    pub fn uses_datadog(&self) -> bool {
        self.source.is_some()
    }
    /// The tag on every metric this run publishes.
    pub fn simulation_run(&self) -> &str {
        &self.run
    }
    fn horizon(&self) -> u64 {
        presets::horizon_ms(self.uses_datadog())
    }
    fn refresh_telemetry(&mut self) {
        let (phase, d) = self.engine.state();
        self.telemetry.sync(phase, &d);
    }
    // Play, pause and reset start a fresh collection boundary: nothing observed before it,
    // or too soon after it, is used as evidence or as forecast history.
    fn clear_evidence(&mut self) {
        self.forecast.reset();
        self.pending = None;
        self.fetch = None;
        self.cached = None;
        self.last_fetch = None;
        self.not_before = crate::datadog::unix_ms() + RESUME_MARGIN_MS;
        self.telemetry_status = COLLECTING.into();
    }
    async fn poll_evidence(&mut self) {
        if self.fetch.as_ref().is_some_and(|f| f.task.is_finished()) {
            let mut fetch = self.fetch.take().unwrap();
            match (&mut fetch.task).await {
                Ok(Ok(evidence)) => {
                    self.telemetry_status = "Datadog evidence ready".into();
                    self.cached = Some(evidence);
                }
                // A failed refresh leaves any still-valid cached observations in place.
                Ok(Err(reason)) => {
                    self.telemetry_status = format!("Deciding from the current cluster; {reason}");
                }
                Err(_) => {
                    self.telemetry_status =
                        "Deciding from the current cluster; Datadog refresh failed".into();
                }
            }
        }
        if self.fetch.is_some()
            || self
                .last_fetch
                .is_some_and(|t| t.elapsed() < FETCH_INTERVAL)
        {
            return;
        }
        let Some(source) = self.source.clone() else {
            return;
        };
        let (run, not_before) = (self.run.clone(), self.not_before);
        self.last_fetch = Some(Instant::now());
        self.fetch = Some(Fetch {
            task: tokio::spawn(async move { source.fetch_autoscaler(&run, not_before).await }),
        });
    }
    /// Cached Datadog observations if they are still valid for this run, stamped with their
    /// age. Invalid ones are dropped; their absence never blocks an evaluation.
    fn usable_telemetry(&mut self) -> Option<datadog::TelemetryEvidence> {
        let telemetry = self.cached.clone()?;
        let now = crate::datadog::unix_ms();
        match telemetry.validate(&self.run, self.not_before, now) {
            Ok(()) => {
                let telemetry = telemetry.at(now);
                self.telemetry_status = format!(
                    "Current cluster state + Datadog context ({:.0}s old)",
                    telemetry.age_seconds
                );
                Some(telemetry)
            }
            Err(reason) => {
                self.cached = None;
                self.telemetry_status = format!("Deciding from the current cluster; {reason}");
                None
            }
        }
    }
    pub fn view(&self) -> View {
        let (phase, d) = self.engine.state();
        let pending_pods = d.pending_count();
        let provisioning = d.count(None, NodePhase::Provisioning);
        let status = if let Some(error) = &self.error {
            error.clone()
        } else if d.at_ms >= self.horizon() {
            "Run complete; reset to start again".into()
        } else if self.pending.is_some() {
            "Jev is evaluating; the cluster keeps running".into()
        } else if self.paused {
            "Paused · start or step the run".into()
        } else if phase == Phase::ScalingUp {
            format!("Scaling up · {provisioning} provisioning")
        } else if phase == Phase::ScalingDown {
            "Scaling down · draining one node".into()
        } else if pending_pods > 0 {
            format!("{pending_pods} pending pods are waiting for a node")
        } else {
            "Stable · every pod is placed".into()
        };
        // Exactly what the next evaluation would be told about the forecast.
        let forecast_headroom = judge::evidence(phase, &d)
            .with_forecast(self.forecast.evidence(d.at_ms))
            .forecast_headroom;
        // Every live node, plus the most recently removed ones for context.
        let nodes = d
            .nodes
            .iter()
            .filter(|n| n.phase != NodePhase::Removed)
            .chain(
                d.nodes
                    .iter()
                    .rev()
                    .filter(|n| n.phase == NodePhase::Removed)
                    .take(6),
            )
            .cloned()
            .collect();
        let now = crate::datadog::unix_ms();
        View {
            scenario: self.scenario,
            scenario_description: self.scenario.description(self.uses_datadog()),
            forecast_headroom,
            forecast: self.forecast.view(d.at_ms),
            at_ms: d.at_ms,
            horizon_ms: self.horizon(),
            seed: self.seed,
            paused: self.paused,
            speed: self.speed,
            available: self.evaluator.is_some(),
            model: self.settings.model.clone(),
            phase,
            revision: d.revision,
            workloads: d
                .workloads
                .iter()
                .enumerate()
                .map(|(i, w)| WorkloadView {
                    desired: w.desired(),
                    pod_memory_gib: w.pod_memory_gib(),
                    available: d.available(i),
                    pending: d
                        .pods
                        .iter()
                        .filter(|p| p.workload == i && p.node.is_none())
                        .count(),
                    min_available: w.min_available(),
                    workload: w.clone(),
                })
                .collect(),
            groups: d
                .groups
                .iter()
                .map(|g| GroupView {
                    ready: d.count(Some(g.group), NodePhase::Ready),
                    provisioning: d.count(Some(g.group), NodePhase::Provisioning),
                    draining: d.count(Some(g.group), NodePhase::Draining),
                    group: g.clone(),
                })
                .collect(),
            nodes,
            node_budget: d.node_budget,
            active_nodes: d.active_nodes(),
            pending_pods,
            oldest_pending_ms: d.oldest_pending_ms(),
            pending_pod_seconds: d.pending_pod_ms as f64 / 1000.,
            requested: d.requested(),
            ready_capacity: d.ready_capacity(),
            hourly_usd: d.hourly_usd(),
            cluster_cost_usd: d.cost_usd(),
            provision_ms: engine::PROVISION_MS,
            scale_down_cooldown_ms: engine::SCALE_DOWN_COOLDOWN_MS,
            legal_actions: engine::legal_choices(phase, &d),
            history: self.history.clone(),
            transitions: self
                .timeline
                .iter()
                .rev()
                .filter_map(|e| match e {
                    TimelineEvent::Transition(t) => Some(t.clone()),
                    _ => None,
                })
                .take(40)
                .collect(),
            controls: self
                .timeline
                .iter()
                .rev()
                .filter_map(|e| match e {
                    TimelineEvent::Control(c) => Some(c.clone()),
                    _ => None,
                })
                .take(40)
                .collect(),
            decisions: self.decisions.iter().rev().take(30).cloned().collect(),
            pending: self.pending.as_ref().map(|p| PendingStatus {
                observed_at_ms: p.evidence.observed_at_ms,
                response_ready: p.task.is_finished(),
            }),
            calls: self.calls,
            cost: self.cost.lock().unwrap().clone(),
            status,
            evidence_source: if self.uses_datadog() {
                "datadog"
            } else {
                "local"
            },
            simulation_run: self.run.clone(),
            telemetry_status: self.uses_datadog().then(|| self.telemetry_status.clone()),
            telemetry: self
                .cached
                .clone()
                .filter(|t| t.validate(&self.run, self.not_before, now).is_ok())
                .map(|t| t.at(now)),
            error: self.error.clone(),
            pods: d.pods,
        }
    }
    pub fn export(&self) -> serde_json::Value {
        let (phase, d) = self.engine.state();
        serde_json::json!({"model":"reflex-autoscaler-v1","seed":self.seed,"scenario":self.scenario,"phase":phase,"state":d,
            "decisions":self.decisions,"timeline":self.timeline,"history":self.history,"forecast":self.forecast.view(d.at_ms),
            "evidence_source":if self.uses_datadog() {"datadog"} else {"local"},"simulation_run":self.run,
            "jev":{"model":self.settings.model,"calls":self.calls,"cost":self.cost.lock().unwrap().clone()}})
    }
    fn reset(&mut self) -> Result<(), Error> {
        // Dropping a pending evaluation aborts it: an old answer cannot reach the new run.
        self.pending = None;
        self.forecast.reset();
        self.forecast.local_min_samples = self.scenario.forecast_min_samples();
        // A new run gets a new tag, so its metrics never mix with the previous run's.
        self.run = datadog::new_run();
        if self.uses_datadog() {
            self.clear_evidence();
        }
        self.engine = Engine::with_meter(self.seed, self.meter.clone())?;
        self.telemetry = telemetry::Telemetry::new(&self.meter, &self.run);
        self.script = self.scenario.script(self.seed, self.uses_datadog());
        self.script_cursor = 0;
        self.paused = true;
        self.last_dispatch = None;
        self.next_due_ms = 0;
        self.calls = 0;
        self.decisions.clear();
        self.timeline.clear();
        self.history.clear();
        self.error = None;
        self.refresh_telemetry();
        Ok(())
    }
    pub async fn command(&mut self, command: Command) -> Result<(), Error> {
        let operation = match &command {
            Command::Scenario { .. } => "scenario",
            Command::Forecast { .. } => "forecast",
            Command::Play => "play",
            Command::Pause => "pause",
            Command::Step => "step",
            Command::Speed { .. } => "speed",
            Command::Control { .. } => "control",
            Command::Reset => "reset",
        };
        // Telemetry is observed in wall-clock time, so the run must keep pace with it.
        if self.uses_datadog() && matches!(command, Command::Step | Command::Speed { value: 2.. }) {
            return Err(Error::Invalid(
                "Datadog evidence requires continuous 1× playback".into(),
            ));
        }
        match command {
            Command::Scenario { scenario } => {
                self.scenario = scenario;
                self.reset()?;
            }
            Command::Forecast { enabled } => {
                if enabled && self.forecast.provider.is_none() {
                    return Err(Error::Invalid(
                        "Toto is not configured on this server".into(),
                    ));
                }
                self.pending = None;
                self.forecast.set_enabled(enabled);
            }
            Command::Play => {
                if self.error.is_some() || self.data().at_ms >= self.horizon() {
                    return Err(Error::Invalid("Reset before resuming this run".into()));
                }
                if self.paused && self.uses_datadog() {
                    self.clear_evidence();
                }
                self.paused = false;
            }
            Command::Pause => {
                self.paused = true;
                if self.uses_datadog() {
                    self.clear_evidence();
                }
            }
            Command::Step => {
                if self.error.is_some() {
                    return Err(Error::Invalid("Reset after an engine error".into()));
                }
                self.paused = true;
                self.advance(1000).await?;
                self.infer().await?;
            }
            Command::Speed { value } => {
                if ![1, 2, 4].contains(&value) {
                    return Err(Error::Invalid("Speed must be 1, 2 or 4".into()));
                }
                self.speed = value;
            }
            Command::Control { control } => {
                let d = self.data();
                if d.at_ms >= self.horizon() {
                    return Err(Error::Invalid(
                        "Reset before changing a completed run".into(),
                    ));
                }
                if let Control::ReplicaSurge { workload, .. }
                | Control::MemoryHeavy { workload, .. } = control
                {
                    if workload >= d.workloads.len() {
                        return Err(Error::Invalid("Unknown workload".into()));
                    }
                }
                // A pending recommendation is left to finish: the freshness guard rejects it
                // if this change moved the cluster revision.
                self.engine.control(control).await?;
                self.timeline.push(TimelineEvent::Control(ControlChange {
                    at_ms: d.at_ms,
                    control,
                    source: "user",
                }));
                self.collect();
            }
            Command::Reset => self.reset()?,
        }
        self.refresh_telemetry();
        tracing::info!(target: "reflex_sim::autoscaler", operation, simulation_run = self.run.as_str(),
            simulation_time_ms = self.data().at_ms, "Cluster autoscaler control applied");
        Ok(())
    }
    pub async fn tick(&mut self, ms: u64) -> Result<(), Error> {
        if !self.paused {
            self.advance(ms * self.speed as u64).await?;
            self.infer().await?;
        }
        Ok(())
    }
    fn collect(&mut self) {
        self.timeline.extend(
            self.engine
                .take_log()
                .into_iter()
                .map(TimelineEvent::Transition),
        );
    }
    /// Advance the clock, stopping at every scripted change and every whole second so that
    /// presets and samples land on exact simulated times whatever the tick size.
    async fn advance(&mut self, ms: u64) -> Result<(), Error> {
        let horizon = self.horizon();
        let target = (self.data().at_ms + ms).min(horizon);
        loop {
            let now = self.data().at_ms;
            if now >= target {
                break;
            }
            let mut stop = target.min((now / 1000 + 1) * 1000);
            if let Some((at, _)) = self.script.get(self.script_cursor) {
                stop = stop.min((*at).max(now));
            }
            // Nodes finishing at this instant are ready before the load changes.
            self.engine.advance(stop).await?;
            let lifecycle = self.engine.take_log();
            if !lifecycle.is_empty() {
                // A node became ready, failed or drained: look at the cluster again at once.
                self.next_due_ms = stop;
                self.timeline
                    .extend(lifecycle.into_iter().map(TimelineEvent::Transition));
            }
            while let Some((at, change)) = self
                .script
                .get(self.script_cursor)
                .copied()
                .filter(|(at, _)| *at <= stop)
            {
                match change {
                    Change::Control(control) => {
                        self.engine.control(control).await?;
                        self.timeline.push(TimelineEvent::Control(ControlChange {
                            at_ms: at,
                            control,
                            source: "preset",
                        }));
                    }
                    Change::Replicas { workload, replicas } => {
                        self.engine.set_replicas(workload, replicas).await?;
                        self.timeline.push(TimelineEvent::Demand {
                            at_ms: at,
                            workload,
                            replicas,
                        });
                    }
                }
                self.script_cursor += 1;
            }
            self.collect();
            self.refresh_telemetry();
            if stop.is_multiple_of(1000) {
                self.sample();
            }
        }
        // In Datadog mode Toto's history is the run's own metrics, queried back.
        let remote = self
            .source
            .clone()
            .map(|source| (source, self.run.clone(), self.not_before, "autoscaler"));
        self.forecast.poll(target, remote).await;
        if target >= horizon {
            self.paused = true;
            if self.uses_datadog() {
                self.clear_evidence();
            }
        }
        Ok(())
    }
    /// One observation per simulated second, for the charts and for Toto.
    fn sample(&mut self) {
        let d = self.data();
        let (requested, ready) = (d.requested(), d.ready_capacity());
        let pending_pods = d.pending_count();
        self.history.push(Sample {
            at_ms: d.at_ms,
            requested_cpu: requested.cpu,
            requested_memory_gib: requested.memory_gib,
            ready_cpu: ready.cpu,
            ready_memory_gib: ready.memory_gib,
            pending_pods,
            nodes: d.active_nodes(),
            provisioning: d.count(None, NodePhase::Provisioning),
        });
        self.forecast.sample(
            d.at_ms,
            [
                requested.cpu as f32,
                requested.memory_gib as f32,
                pending_pods as f32,
            ],
            SERIES,
        );
    }
    async fn record(
        &mut self,
        id: u64,
        evidence: Evidence,
        result: Inference,
    ) -> Result<(), Error> {
        let outcome = match (&result.error, result.choice) {
            (Some(error), _) => {
                self.engine
                    .evaluation_error(&error.code, &error.message)
                    .await?
            }
            (None, Some(choice)) if !evidence.legal_actions.contains(&choice) => {
                let phase = self.phase();
                Outcome {
                    status: "rejected",
                    code: Some("not_offered".into()),
                    reason: "not_offered: The action was not among the legal actions".into(),
                    from: phase,
                    to: phase,
                }
            }
            (None, Some(choice)) => {
                let action = Action {
                    choice,
                    revision: evidence.revision,
                    observed_at: evidence.observed_at_ms,
                    headroom: evidence.headroom(),
                    headroom_expires_at: evidence.headroom_expires_at_ms,
                };
                self.engine.apply(action, result.confidence).await?
            }
            (None, None) => {
                self.engine
                    .evaluation_error(
                        "missing_action",
                        "Jev supplied neither an action nor an error",
                    )
                    .await?
            }
        };
        self.collect();
        self.refresh_telemetry();
        self.telemetry.decision(result.choice, &outcome);
        tracing::info!(target: "reflex_sim::autoscaler", decision_id = id, simulation_run = self.run.as_str(),
            action = result.choice.map(|c| c.label()), outcome = outcome.status, guard = outcome.code.as_deref(),
            "Cluster autoscaler recommendation processed");
        let decision = Decision {
            id,
            at_ms: self.data().at_ms,
            observed_at_ms: evidence.observed_at_ms,
            choice: result.choice,
            status: outcome.status,
            code: outcome.code,
            reason: outcome.reason,
            from: outcome.from,
            to: outcome.to,
            evidence,
            result,
        };
        self.timeline
            .push(TimelineEvent::Decision(Box::new(decision.clone())));
        self.decisions.push(decision);
        Ok(())
    }
    /// Apply a finished recommendation, then ask for the next one. The request runs in its
    /// own task: the caller holds the session lock only to start it and to apply its result.
    async fn infer(&mut self) -> Result<(), Error> {
        if self.uses_datadog() {
            if self.paused || self.data().at_ms >= self.horizon() {
                return Ok(());
            }
            self.poll_evidence().await;
        }
        if self.pending.as_ref().is_some_and(|p| p.task.is_finished()) {
            let mut pending = self.pending.take().unwrap();
            let result = match (&mut pending.task).await {
                Ok(result) => result,
                Err(_) => {
                    Inference::failed("worker_failed", "Jev evaluation task did not complete")
                }
            };
            let span = pending.trace.child("apply");
            let recorded = pending
                .trace
                .scope(
                    self.record(pending.id, pending.evidence.clone(), result)
                        .instrument(span),
                )
                .await;
            pending.trace.finish(match &recorded {
                Ok(()) => self.decisions.last().map_or("error", |d| d.status),
                Err(_) => "error",
            });
            recorded?;
        }
        let (phase, d) = self.engine.state();
        if self.pending.is_some() || d.at_ms >= self.horizon() || d.at_ms < self.next_due_ms {
            return Ok(());
        }
        let Some(evaluator) = self.evaluator.clone() else {
            return Ok(());
        };
        if self
            .last_dispatch
            .is_some_and(|t| t.elapsed() < self.settings.dispatch_interval)
        {
            return Ok(());
        }
        let mut evidence =
            judge::evidence(phase, &d).with_forecast(self.forecast.evidence(d.at_ms));
        // With a single legal action there is nothing to decide, and no call is made.
        if evidence.legal_actions.len() < 2 {
            return Ok(());
        }
        // Datadog observations are attached when valid. The live cluster state above is
        // always sent, so missing or stale telemetry never delays or changes an evaluation.
        if self.uses_datadog() {
            evidence.telemetry = self.usable_telemetry();
        }
        // Jev also sees what Reflex recently refused. Stale answers and cooldown refusals are
        // left out: neither says anything about the action once time has passed.
        evidence.recent_rejections = self
            .decisions
            .iter()
            .rev()
            .take_while(|r| d.at_ms - r.at_ms <= REJECTION_MEMORY_MS)
            .filter(|r| {
                r.status == "rejected"
                    && !matches!(r.code.as_deref(), Some("fresh" | "scale_down_cooldown"))
            })
            .filter_map(|r| {
                Some(judge::RejectionEvidence {
                    action: r.choice?,
                    guard: r.code.clone()?,
                    seconds_ago: (d.at_ms - r.at_ms) / 1000,
                })
            })
            .take(3)
            .collect();
        let captured = evidence.clone();
        let cost = self.cost.clone();
        cost.lock().unwrap().dispatched();
        self.calls += 1;
        self.last_dispatch = Some(Instant::now());
        self.next_due_ms = d.at_ms + EVALUATION_INTERVAL_MS;
        let trace = DecisionTrace::autoscaler(
            self.calls as u64,
            telemetry::phase(phase),
            &self.run,
            d.at_ms,
        );
        let span = trace.child("evaluate");
        let task = tokio::spawn(
            trace.scope(
                async move {
                    let result = evaluator.evaluate(captured).await;
                    // Meter on arrival, including while paused or after a later guard rejection.
                    cost.lock()
                        .unwrap()
                        .record_usage(result.model.as_deref(), result.usage.as_ref());
                    result
                }
                .instrument(span),
            ),
        );
        self.pending = Some(Pending {
            id: self.calls as u64,
            evidence,
            task,
            trace,
        });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capacity::forecast::{ForecastFuture, Forecaster, Input, Series, Snapshot};

    /// Predicts 30 requested CPU and 60 GiB throughout: 12 CPU and 20 GiB above today.
    struct Rising;
    impl Forecaster for Rising {
        fn description(&self) -> String {
            "Test forecaster".into()
        }
        fn forecast(&self, input: Input) -> ForecastFuture<'_> {
            let flat = |value: f32| Series {
                median: vec![value; input.prediction_length],
                lower: vec![value; input.prediction_length],
                upper: vec![value; input.prediction_length],
            };
            let snapshot = Snapshot {
                origin_ms: input.origin_ms,
                interval_ms: input.interval_ms,
                request_id: "fixture".into(),
                source: "test".into(),
                model_provenance: "test only".into(),
                quantiles: [0.1, 0.5, 0.9],
                series: vec![flat(30.), flat(60.), flat(0.)],
                latency_ms: 1.,
            };
            Box::pin(async move { Ok(snapshot) })
        }
    }
    /// Always asks for one more large node, and records what it was shown.
    struct Eager(Mutex<Vec<serde_json::Value>>);
    impl Evaluator for Eager {
        fn evaluate(&self, evidence: Evidence) -> judge::Evaluation<'_> {
            let wanted = Choice::ScaleUp {
                group: engine::Group::GeneralLarge,
                count: 1,
            };
            let choice = if evidence.legal_actions.contains(&wanted) {
                wanted
            } else {
                Choice::NoChange
            };
            self.0
                .lock()
                .unwrap()
                .push(serde_json::to_value(&evidence).unwrap());
            Box::pin(async move { Inference::chose(choice, Some(0.9)) })
        }
    }
    async fn step(s: &mut Session, seconds: usize) {
        for _ in 0..seconds {
            s.command(Command::Step).await.unwrap();
            tokio::task::yield_now().await;
        }
    }

    #[tokio::test]
    async fn a_fresh_forecast_reaches_jev_and_justifies_provisioning_ahead_of_demand() {
        let judge = Arc::new(Eager(Mutex::new(vec![])));
        let settings = JevSettings {
            dispatch_interval: std::time::Duration::ZERO,
            ..Default::default()
        };
        let mut s = Session::new(42, Some(judge.clone()), settings).unwrap();
        s.forecast.provider = Some(Arc::new(Rising));

        // Without a forecast nothing is pending, so no scale-up is justified.
        step(&mut s, 63).await;
        assert!(!s.decisions().is_empty());
        assert!(s
            .decisions()
            .iter()
            .all(|d| d.code.as_deref() == Some("justified_scale_up")));
        assert!(judge
            .0
            .lock()
            .unwrap()
            .iter()
            .all(|e| e.get("forecast").is_none() && e.get("forecast_headroom").is_none()));
        assert_eq!(s.data().active_nodes(), 4);
        assert_eq!(s.view().forecast.samples, 63);

        step(&mut s, 20).await;
        let seen = judge.0.lock().unwrap().clone();
        let with_forecast = seen.iter().find(|e| e.get("forecast").is_some()).unwrap();
        assert_eq!(
            with_forecast["forecast"]["source"],
            "simulator_observations"
        );
        let names: Vec<_> = with_forecast["forecast"]["series"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, SERIES);
        assert_eq!(
            with_forecast["forecast_headroom"],
            serde_json::json!({"cpu":12,"memory_gib":20})
        );
        // Two large nodes cover the forecast rise; a third is refused.
        let d = s.data();
        assert_eq!(s.phase(), Phase::ScalingUp);
        assert_eq!(d.count(None, NodePhase::Provisioning), 2);
        let last = s.decisions().last().unwrap();
        assert_eq!(last.code.as_deref(), Some("justified_scale_up"));
        // The forecast justified provisioning; it placed nothing and added no usable capacity.
        assert_eq!(d.pending_count(), 0);
        assert_eq!(d.ready_capacity().cpu, 24);
        assert_eq!(s.view().forecast_headroom.unwrap().cpu, 12);

        let off = Command::Forecast { enabled: false };
        s.command(off).await.unwrap();
        step(&mut s, 6).await;
        let seen = judge.0.lock().unwrap().clone();
        assert!(seen.last().unwrap().get("forecast").is_none());
        assert!(s.view().forecast_headroom.is_none());
        s.command(Command::Reset).await.unwrap();
        let view = s.view();
        assert!(view.forecast.forecast.is_none() && view.forecast.configured);
        assert_eq!((view.forecast.samples, view.forecast.calls), (0, 0));
        assert!(!view.forecast.enabled, "the switch survives a reset");
    }
}

#[cfg(test)]
mod datadog_tests {
    use super::*;
    use crate::capacity::forecast::{ForecastFuture, Forecaster, Input, Series, Snapshot};
    use crate::datadog::{unix_ms, Source};
    use engine::{Group, WORKLOADS};
    use serde_json::{json, Value};

    const GIB: f64 = 1073741824.;
    /// Records what it is shown; asks for one more large node whenever that is offered.
    struct Judge(Mutex<Vec<Value>>);
    impl Evaluator for Judge {
        fn evaluate(&self, evidence: Evidence) -> judge::Evaluation<'_> {
            let wanted = Choice::ScaleUp {
                group: Group::GeneralLarge,
                count: 1,
            };
            let offered = evidence.legal_actions.contains(&wanted);
            self.0
                .lock()
                .unwrap()
                .push(serde_json::to_value(&evidence).unwrap());
            let choice = if offered { wanted } else { Choice::NoChange };
            Box::pin(async move { Inference::chose(choice, Some(0.9)) })
        }
    }
    /// Predicts 30 requested CPU and 60 GiB on whatever history it is given, and keeps it.
    struct Recorded(Mutex<Vec<Input>>);
    impl Forecaster for Recorded {
        fn description(&self) -> String {
            "Test forecaster".into()
        }
        fn forecast(&self, input: Input) -> ForecastFuture<'_> {
            let flat = |value: f32| Series {
                median: vec![value; input.prediction_length],
                lower: vec![value; input.prediction_length],
                upper: vec![value; input.prediction_length],
            };
            let snapshot = Snapshot {
                origin_ms: input.origin_ms,
                interval_ms: input.interval_ms,
                request_id: "fixture".into(),
                source: "test".into(),
                model_provenance: "test only".into(),
                quantiles: [0.1, 0.5, 0.9],
                series: vec![flat(30.), flat(60.), flat(0.)],
                latency_ms: 1.,
            };
            self.0.lock().unwrap().push(input);
            Box::pin(async move { Ok(snapshot) })
        }
    }
    fn session(judge: Arc<Judge>, datadog: &str) -> Session {
        let evaluator = Arc::new(datadog::DatadogEvaluator::new(
            judge,
            Source::for_test(datadog),
        ));
        let settings = JevSettings {
            dispatch_interval: std::time::Duration::ZERO,
            ..Default::default()
        };
        Session::new(42, Some(evaluator), settings).unwrap()
    }
    /// What a successful query would have cached, observed `age_ms` ago.
    fn observed(s: &Session, age_ms: u64) -> datadog::TelemetryEvidence {
        let now = unix_ms();
        datadog::TelemetryEvidence {
            source: "datadog".into(),
            simulation_run: s.run.clone(),
            fetched_at_unix_ms: now,
            observed_at_unix_ms: now - age_ms,
            age_seconds: 0.,
            requested_cpu: 18.,
            requested_memory_bytes: 40. * GIB,
            workloads: WORKLOADS
                .iter()
                .map(|w| datadog::WorkloadTelemetry {
                    workload: w.to_string(),
                    desired_replicas: 4.,
                    available_replicas: 4.,
                    pending_pods: 0.,
                    oldest_pending_age_seconds: 0.,
                })
                .collect(),
            groups: Group::ALL
                .iter()
                .map(|g| datadog::GroupTelemetry {
                    group: g.key().into(),
                    ready_nodes: 2.,
                    provisioning_nodes: 0.,
                    draining_nodes: 0.,
                    cpu_capacity: 16.,
                    cpu_reserved: 14.,
                    memory_capacity_bytes: 32. * GIB,
                    memory_reserved_bytes: 30. * GIB,
                })
                .collect(),
        }
    }

    #[tokio::test]
    async fn telemetry_is_attached_when_valid_and_never_blocks_an_evaluation() {
        for context in [
            "missing",
            "stale",
            "wrong_run",
            "expired_query",
            "valid",
            "fetch_failed",
        ] {
            let judge = Arc::new(Judge(Mutex::new(vec![])));
            let mut s = session(judge.clone(), "http://127.0.0.1:1");
            s.command(Command::Play).await.unwrap();
            s.not_before = unix_ms() - 120_000;
            // No real query is made in this test.
            s.last_fetch = Some(Instant::now());
            s.cached = match context {
                "missing" => None,
                "stale" => Some(observed(&s, crate::datadog::MAX_AGE_MS + 1_000)),
                "wrong_run" => Some(datadog::TelemetryEvidence {
                    simulation_run: "autoscaler-other".into(),
                    ..observed(&s, 30_000)
                }),
                "expired_query" => Some(datadog::TelemetryEvidence {
                    fetched_at_unix_ms: unix_ms() - datadog::QUERY_TTL_MS - 1_000,
                    ..observed(&s, 30_000)
                }),
                _ => Some(observed(&s, 30_000)),
            };
            if context == "fetch_failed" {
                s.fetch = Some(Fetch {
                    task: tokio::spawn(async { Err("test refresh failure".into()) }),
                });
                while !s.fetch.as_ref().unwrap().task.is_finished() {
                    tokio::task::yield_now().await;
                }
            }
            s.tick(1000).await.unwrap();
            assert_eq!(s.calls, 1, "{context}: the evaluation went ahead");
            for _ in 0..5 {
                tokio::task::yield_now().await;
            }
            let seen = judge.0.lock().unwrap()[0].clone();
            // The live cluster is always what Jev is asked about.
            assert_eq!(seen["nodes"].as_array().unwrap().len(), 4, "{context}");
            assert_eq!(seen["requested"]["cpu"], 18, "{context}");
            assert_eq!(seen["pending_pods"], json!([]), "{context}");
            assert!(seen["legal_actions"].as_array().unwrap().len() > 1);
            let usable = matches!(context, "valid" | "fetch_failed");
            assert_eq!(seen.get("telemetry").is_some(), usable, "{context}");
            let status = s.view().telemetry_status.unwrap();
            if usable {
                let telemetry = &seen["telemetry"];
                assert_eq!(telemetry["source"], "datadog");
                assert_eq!(telemetry["simulation_run"], s.run.as_str());
                let age = telemetry["age_seconds"].as_f64().unwrap();
                assert!((30. ..32.).contains(&age), "{context}: {age}");
                assert!(telemetry["observed_at_unix_ms"].as_u64().unwrap() < unix_ms());
                assert!(status.contains("Datadog context (30s old)"), "{status}");
                assert!(s.view().telemetry.is_some());
            } else {
                assert!(
                    status.starts_with("Deciding from the current cluster"),
                    "{status}"
                );
                assert!(
                    s.cached.is_none(),
                    "{context}: unusable evidence is dropped"
                );
                assert!(s.view().telemetry.is_none());
            }
            // The recommendation was unjustified either way: telemetry authorises nothing.
            s.tick(1000).await.unwrap();
            tokio::task::yield_now().await;
            s.tick(1000).await.unwrap();
            assert_eq!(
                s.decisions[0].code.as_deref(),
                Some("justified_scale_up"),
                "{context}"
            );
        }
    }

    #[tokio::test]
    async fn datadog_mode_runs_in_real_time_with_a_fresh_run_per_reset() {
        let judge = Arc::new(Judge(Mutex::new(vec![])));
        let mut s = session(judge, "http://127.0.0.1:1");
        let view = s.view();
        assert_eq!(view.evidence_source, "datadog");
        assert_eq!(view.horizon_ms, presets::DATADOG_HORIZON_MS);
        assert_eq!(view.simulation_run, s.run);
        assert!(view
            .telemetry_status
            .unwrap()
            .contains("collecting Datadog context"));
        for command in [
            Command::Step,
            Command::Speed { value: 2 },
            Command::Speed { value: 4 },
        ] {
            let error = s.command(command).await.unwrap_err().to_string();
            assert!(error.contains("continuous 1× playback"), "{error}");
        }
        s.command(Command::Speed { value: 1 }).await.unwrap();

        // Scheduled changes come at twice their local times, after telemetry has warmed up.
        let first = s.run.clone();
        let surge = Command::Scenario {
            scenario: Scenario::SurgeRecovery,
        };
        s.command(surge).await.unwrap();
        assert_ne!(s.run, first, "a new run gets a new tag");
        assert!(s.run.starts_with("autoscaler-"));
        let times: Vec<u64> = s.script.iter().map(|(at, _)| *at).collect();
        assert_eq!(times, [90_000, 180_000, 480_000, 600_000]);
        assert!(s
            .view()
            .scenario_description
            .contains("at 90s and api at 180s"));
        let cyclical = Scenario::CyclicalLoad.script(42, true);
        let local = Scenario::CyclicalLoad.script(42, false);
        assert_eq!(
            cyclical[..local.len()],
            local[..],
            "the cycle itself is unchanged"
        );
        assert!(cyclical.last().unwrap().0 > engine::HORIZON_MS);
        assert!(cyclical.last().unwrap().0 < presets::DATADOG_HORIZON_MS);

        // Play, pause and reset each start a fresh collection boundary.
        s.cached = Some(observed(&s, 30_000));
        s.command(Command::Play).await.unwrap();
        assert!(s.cached.is_none());
        assert!(s.not_before >= unix_ms() + RESUME_MARGIN_MS - 1_000);
        s.cached = Some(observed(&s, 30_000));
        s.command(Command::Pause).await.unwrap();
        assert!(s.cached.is_none() && s.fetch.is_none() && s.pending.is_none());
        let second = s.run.clone();
        s.command(Command::Reset).await.unwrap();
        assert_ne!(s.run, second);
        // While paused nothing is fetched and nothing is asked.
        s.tick(1000).await.unwrap();
        assert!(s.fetch.is_none() && s.calls == 0);

        // Without Datadog evidence none of this applies.
        let mut local = Session::new(42, None, JevSettings::default()).unwrap();
        assert_eq!(local.view().evidence_source, "local");
        assert_eq!(local.view().horizon_ms, engine::HORIZON_MS);
        assert!(local.view().telemetry_status.is_none());
        local.command(Command::Step).await.unwrap();
        local.command(Command::Speed { value: 4 }).await.unwrap();
        let surge = Command::Scenario {
            scenario: Scenario::SurgeRecovery,
        };
        local.command(surge).await.unwrap();
        assert_eq!(local.script[0].0, 45_000);
    }

    #[tokio::test]
    async fn a_forecast_built_from_datadog_history_justifies_a_scale_up_with_its_provenance() {
        use axum::{routing::post, Json, Router};
        let app = Router::new().route(
            "/api/v2/query/timeseries",
            post(|Json(body): Json<Value>| async move {
                let a = &body["data"]["attributes"];
                let queries = a["queries"].as_array().unwrap();
                if queries.len() != 3 {
                    // Evidence: one complete bucket, thirty seconds behind the wall clock.
                    return Json(datadog::tests::fixture(
                        unix_ms() / 10_000 * 10_000 - 30_000,
                    ));
                }
                // Forecast history: this run's own demand metrics on the ten-second grid.
                let names = [
                    "autoscaler.cpu.requested",
                    "autoscaler.memory.requested",
                    "autoscaler.pods.pending",
                ];
                for (q, name) in queries.iter().zip(names) {
                    let q = q["query"].as_str().unwrap();
                    assert!(q.starts_with(&format!("sum:{name}{{")), "{q}");
                    assert!(q.contains("policy:jev,simulation_run:autoscaler-"), "{q}");
                    assert!(q.ends_with(".rollup(avg,10).fill(null)"), "{q}");
                }
                let (from, to) = (a["from"].as_u64().unwrap(), a["to"].as_u64().unwrap());
                let times: Vec<u64> = (from..to).step_by(10_000).collect();
                let series = |value: f64| vec![value; times.len()];
                Json(json!({"data":{"attributes":{
                    "times":times,
                    "series":[{"query_index":0},{"query_index":1},{"query_index":2}],
                    "values":[series(18.), series(40. * GIB), series(0.)]
                }}}))
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let judge = Arc::new(Judge(Mutex::new(vec![])));
        let toto = Arc::new(Recorded(Mutex::new(vec![])));
        let mut s = session(judge.clone(), &base);
        s.forecast.provider = Some(toto.clone());
        s.command(Command::Play).await.unwrap();
        // As if the run had been playing for long enough to have its history in Datadog.
        s.not_before = unix_ms() - 400_000;
        for _ in 0..400 {
            s.tick(1000).await.unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            if s.decisions.iter().any(|d| d.status == "applied") {
                break;
            }
        }
        let applied = s.decisions.iter().find(|d| d.status == "applied").unwrap();
        assert_eq!(
            applied.choice.map(|c| c.label()).as_deref(),
            Some("scale_up:general_large:1")
        );
        let evidence = serde_json::to_value(&applied.evidence).unwrap();

        // Toto saw 320 seconds of queried history, with memory converted from bytes to GiB.
        let input = toto.0.lock().unwrap()[0].clone();
        assert_eq!((input.interval_ms, input.values.len()), (10_000, 32));
        assert_eq!(input.values[0], vec![18., 40., 0.]);
        assert_eq!(s.view().forecast.source, "datadog_observations");
        assert_eq!(s.view().forecast.series_names, SERIES);

        // The evidence says where the forecast came from and how long it may be used.
        assert_eq!(evidence["forecast"]["source"], "datadog_observations");
        assert_eq!(evidence["forecast"]["history_seconds"], 320);
        let basis = &evidence["forecast_basis"];
        assert_eq!(basis["source"], "datadog_observations");
        assert_eq!(basis["time_domain"], "forecast_epoch_wall_clock");
        assert_eq!(basis["max_age_seconds"], 60);
        assert_eq!(basis["lookahead_seconds"], 60);
        let age = basis["age_seconds"].as_u64().unwrap();
        assert!((20..=60).contains(&age), "{age}");
        assert_eq!(
            evidence["forecast_headroom"],
            json!({"cpu":12,"memory_gib":20})
        );
        assert_eq!(
            applied.evidence.headroom_expires_at_ms,
            applied.evidence.observed_at_ms + 60_000
                - applied.evidence.forecast.as_ref().unwrap().age_ms
        );

        // Delayed telemetry rode along as context; the live cluster is what was decided on.
        assert_eq!(evidence["telemetry"]["source"], "datadog");
        assert_eq!(evidence["telemetry"]["workloads"][0]["pending_pods"], 2.);
        assert_eq!(evidence["pending_pods"], json!([]));
        let age = evidence["telemetry"]["age_seconds"].as_f64().unwrap();
        assert!((20. ..=60.).contains(&age), "{age}");

        // The forecast justified provisioning; it added no usable capacity.
        let d = s.data();
        assert!(d.count(None, NodePhase::Provisioning) >= 1);
        assert_eq!(d.ready_capacity().cpu, 24);
        assert!(s.decisions[0].code.as_deref() == Some("justified_scale_up"));
        server.abort();
    }
}
