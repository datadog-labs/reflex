// Unless explicitly stated otherwise all files in this repository are licensed under the Apache-2.0 License.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-present Datadog, Inc.

//! Non-preemptive resource scheduling with guarded, atomic placement.
pub mod datadog;
pub mod engine;
pub mod judge;
mod presets;
mod telemetry;
pub mod web;
use crate::decision_trace::DecisionTrace;
use crate::{
    playground::inference::{CostStatus, JevSettings},
    Error,
};
pub use engine::Priority;
use engine::{Choice, Data, Engine, Job, JobPhase, Placement, HORIZON_MS};
use judge::{Evaluator, Evidence};
use opentelemetry::metrics::Meter;
pub use presets::Scenario;
use serde::{Deserialize, Serialize};
use std::{
    sync::{Arc, Mutex},
    time::Instant,
};
use tokio::task::JoinHandle;
use tracing::Instrument;
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Policy {
    Jev,
    FirstFit,
    BestFit,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClientConfig {
    pub rate: f64,
    pub cpu: u32,
    pub memory_gib: u32,
    pub duration_ms: u64,
    pub enabled: bool,
}
impl ClientConfig {
    fn validate(&self) -> Result<(), Error> {
        if !self.rate.is_finite()
            || self.rate < 0.
            || self.rate.fract() != 0.
            || !(1..=32).contains(&self.cpu)
            || !(1..=64).contains(&self.memory_gib)
            || !(1000..=30000).contains(&self.duration_ms)
        {
            return Err(Error::Invalid(
                "Client limits: finite nonnegative whole-number jobs/s, 1–32 CPU, 1–64 GiB, 1–30 seconds".into(),
            ));
        }
        Ok(())
    }
    fn next(&self, at: u64) -> Option<f64> {
        self.next_fractional(at as f64)
    }
    fn next_fractional(&self, at: f64) -> Option<f64> {
        if self.enabled && self.rate > 0. {
            let interval = 1000. / self.rate;
            // Preserve the existing millisecond spacing for slower sources; retain
            // fractional deadlines for multiple arrivals per millisecond at high rates.
            let interval = if interval >= 1. {
                interval.round()
            } else {
                interval
            };
            let next = (at + interval).max(at.next_up());
            (next.is_finite() && next < u64::MAX as f64).then_some(next)
        } else {
            None
        }
    }
}
#[derive(Clone, Serialize)]
pub struct Client {
    pub priority: Priority,
    pub id: u64,
    pub name: String,
    pub config: ClientConfig,
    #[serde(skip_serializing)]
    next_at: Option<f64>,
    #[serde(skip_serializing)]
    sequence: u64,
}
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Command {
    Scenario { scenario: Scenario },
    Forecast { enabled: bool },
    Play,
    Pause,
    Step,
    Reset,
    Speed { value: u8 },
    Policy { policy: Policy },
    AddClient,
    RemoveClient { id: u64 },
    Priority { id: u64, priority: Priority },
    Client { id: u64, config: ClientConfig },
}
#[derive(Clone, Serialize)]
pub struct Decision {
    pub at_ms: u64,
    pub job: u64,
    pub policy: Policy,
    pub choice: Option<Choice>,
    pub status: String,
    pub reason: String,
    pub evidence: Option<Evidence>,
    pub result: Option<judge::Inference>,
}
#[derive(Clone, Serialize)]
pub struct Sample {
    pub nodes: Vec<engine::Node>,
    pub clients: Vec<engine::ClientLag>,
    pub at_ms: u64,
    pub queued: usize,
    pub running: usize,
    pub completed: usize,
}
#[derive(Serialize)]
pub struct View {
    pub live_lag: Vec<engine::ClientLag>,
    pub priority_changes: Vec<PriorityChange>,
    pub client_stats: Vec<engine::ClientStats>,
    pub client_priorities: std::collections::BTreeMap<u64, Priority>,
    pub eligible_jobs: Vec<u64>,
    pub aging_ms: u64,
    pub scenario: Scenario,
    pub scenario_description: String,
    pub forecast: crate::forecasting::View,
    pub at_ms: u64,
    pub horizon_ms: u64,
    pub seed: u64,
    pub paused: bool,
    pub speed: u8,
    pub policy: Policy,
    pub available: bool,
    pub clients: Vec<Client>,
    pub nodes: Vec<engine::Node>,
    pub jobs: Vec<Job>,
    pub queued: usize,
    pub running: usize,
    pub completed: usize,
    pub rejected: usize,
    pub mean_wait_ms: f64,
    pub oldest_wait_ms: u64,
    pub history: Vec<Sample>,
    pub decisions: Vec<Decision>,
    pub pending_job: Option<u64>,
    pub calls: usize,
    pub cost: CostStatus,
    pub status: String,
    pub evidence_source: String,
    pub simulation_run: Option<String>,
    pub telemetry_status: Option<String>,
    pub error: Option<String>,
}
struct Pending {
    evidence: Evidence,
    task: JoinHandle<judge::Inference>,
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
#[derive(Clone, Serialize)]
pub struct PriorityChange {
    pub at_ms: u64,
    pub client: u64,
    pub priority: Priority,
}
pub struct Session {
    priority_changes: Vec<PriorityChange>,
    scenario: Scenario,
    script_cursor: usize,
    pub(crate) forecast: crate::forecasting::Driver,
    source: Option<Arc<crate::datadog::Source>>,
    run: Option<String>,
    not_before: u64,
    fetch: Option<Fetch>,
    last_fetch: Option<Instant>,
    cached: Option<datadog::TelemetryEvidence>,
    telemetry_status: String,
    engine: Engine,
    meter: Meter,
    telemetry: telemetry::Telemetry,
    seed: u64,
    clients: Vec<Client>,
    next_client: u64,
    next_job: u64,
    paused: bool,
    speed: u8,
    policy: Policy,
    evaluator: Option<Arc<dyn Evaluator>>,
    settings: JevSettings,
    pending: Option<Pending>,
    last_dispatch: Option<Instant>,
    calls: usize,
    cost: Arc<Mutex<CostStatus>>,
    decisions: Vec<Decision>,
    history: Vec<Sample>,
    edits: Vec<serde_json::Value>,
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
            opentelemetry::global::meter("scheduler"),
        )
    }
    /// Configure metrics with an application-owned meter before starting the session.
    pub fn with_meter(
        seed: u64,
        evaluator: Option<Arc<dyn Evaluator>>,
        settings: JevSettings,
        meter: Meter,
    ) -> Result<Self, Error> {
        // At 1 job/s per client, these jobs offer 12 CPU-seconds/s and
        // 24 GiB-seconds/s: one third of the pool, before runtime variation.
        let clients = [(1., 1, 2, 2000), (1., 2, 4, 2000), (1., 3, 6, 2000)]
            .into_iter()
            .enumerate()
            .map(|(i, (rate, cpu, memory_gib, duration_ms))| {
                let config = ClientConfig {
                    rate,
                    cpu,
                    memory_gib,
                    duration_ms,
                    enabled: true,
                };
                Client {
                    priority: Priority::Normal,
                    id: i as u64,
                    name: format!("Client {}", i + 1),
                    next_at: config.next(0),
                    config,
                    sequence: 0,
                }
            })
            .collect();
        let policy = if evaluator.is_some() {
            Policy::Jev
        } else {
            Policy::BestFit
        };
        let source = evaluator.as_ref().and_then(|e| e.evidence_source());
        let run = source.as_ref().map(|_| datadog::new_run());
        let mut session = Self {
            priority_changes: vec![],
            scenario: Scenario::Sandbox,
            script_cursor: 0,
            source,
            run: run.clone(),
            not_before: crate::datadog::unix_ms(),
            fetch: None,
            last_fetch: None,
            cached: None,
            telemetry_status: "Scheduling from current state; collecting Datadog context".into(),
            engine: Engine::new()?,
            telemetry: telemetry::Telemetry::new(meter.clone(), policy, run),
            meter,
            seed,
            clients,
            next_client: 3,
            next_job: 1,
            paused: true,
            speed: 1,
            policy,
            evaluator,
            settings,
            forecast: Default::default(),
            pending: None,
            last_dispatch: None,
            calls: 0,
            cost: Arc::new(Mutex::new(CostStatus::default())),
            decisions: vec![],
            history: vec![],
            edits: vec![],
            error: None,
        };
        session.refresh_telemetry();
        Ok(session)
    }
    fn horizon(&self) -> u64 {
        if self.uses_datadog() {
            600_000
        } else if self.scenario == Scenario::Cyclical {
            360_000
        } else {
            HORIZON_MS
        }
    }
    pub fn uses_datadog(&self) -> bool {
        self.source.is_some()
    }
    fn clear_evidence(&mut self) {
        self.forecast.reset();
        self.pending = None;
        self.fetch = None;
        self.cached = None;
        self.last_fetch = None;
        self.not_before = crate::datadog::unix_ms() + 20_000;
        self.telemetry_status = "Scheduling from current state; collecting Datadog context".into();
    }
    fn telemetry_clients(&self) -> Vec<u64> {
        self.clients
            .iter()
            .map(|c| c.id)
            .chain(self.data().jobs.iter().map(|j| j.client))
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect()
    }
    async fn poll_evidence(&mut self) {
        if self.fetch.as_ref().is_some_and(|f| f.task.is_finished()) {
            let mut fetch = self.fetch.take().unwrap();
            match (&mut fetch.task).await {
                Ok(Ok(e)) => {
                    self.telemetry_status = "Datadog evidence ready".into();
                    self.cached = Some(e);
                }
                Ok(Err(reason)) => {
                    self.telemetry_status = format!("Scheduling from current state; {reason}");
                }
                Err(_) => {
                    self.telemetry_status =
                        "Scheduling from current state; Datadog refresh failed".into();
                }
            }
        }
        if self.fetch.is_some()
            || self
                .last_fetch
                .is_some_and(|t| t.elapsed() < std::time::Duration::from_secs(10))
        {
            return;
        }
        let Some(source) = self.source.clone() else {
            return;
        };
        let run = self.run.clone().unwrap();
        let clients = self.telemetry_clients();
        let not_before = self.not_before;
        self.last_fetch = Some(Instant::now());
        self.fetch = Some(Fetch {
            task: tokio::spawn(
                async move { source.fetch_scheduler(&run, &clients, not_before).await },
            ),
        });
    }
    fn refresh_telemetry(&mut self) {
        self.telemetry
            .sync(&self.engine.data(), self.clients.iter().map(|c| c.id));
    }
    pub fn data(&self) -> Data {
        self.engine.data()
    }
    pub fn view(&self) -> View {
        let d = self.data();
        let queued = d
            .jobs
            .iter()
            .filter(|j| j.phase == JobPhase::Queued)
            .count();
        let running = d
            .jobs
            .iter()
            .filter(|j| j.phase == JobPhase::Running)
            .count();
        let completed = d
            .jobs
            .iter()
            .filter(|j| j.phase == JobPhase::Completed)
            .count();
        let rejected = d
            .jobs
            .iter()
            .filter(|j| j.phase == JobPhase::Rejected)
            .count();
        let waits: Vec<_> = d
            .jobs
            .iter()
            .filter_map(|j| j.started_at.map(|t| t - j.arrived_at))
            .collect();
        let oldest_wait_ms = d
            .jobs
            .iter()
            .filter(|j| j.phase == JobPhase::Queued)
            .map(|j| d.at_ms - j.arrived_at)
            .max()
            .unwrap_or(0);
        let status = if self.pending.is_some() {
            "Jev is evaluating; arrivals and running jobs continue"
        } else if d.at_ms >= self.horizon() {
            "Session complete; unfinished jobs remain in the snapshot"
        } else if queued > 0 && judge::evidence(&d).is_none() {
            "Client queue heads are waiting for capacity"
        } else if self.paused {
            "Paused · configure clients, then start or step"
        } else if self.policy == Policy::Jev {
            "Waiting for the next eligible placement evaluation"
        } else {
            "Placing client queue heads by priority, with aging protection"
        }
        .into();
        // Keep all live jobs, plus the latest 40 terminal jobs. Export retains the full run.
        let jobs = d
            .jobs
            .iter()
            .filter(|j| matches!(j.phase, JobPhase::Queued | JobPhase::Running))
            .chain(
                d.jobs
                    .iter()
                    .rev()
                    .filter(|j| matches!(j.phase, JobPhase::Completed | JobPhase::Rejected))
                    .take(40),
            )
            .cloned()
            .collect();
        View {
            live_lag: self
                .telemetry_clients()
                .iter()
                .map(|id| d.client_lag(*id))
                .collect(),
            priority_changes: self
                .priority_changes
                .iter()
                .filter(|e| e.at_ms >= d.at_ms.saturating_sub(60_000))
                .cloned()
                .collect(),
            client_stats: self
                .telemetry_clients()
                .iter()
                .map(|id| d.client_stats(*id))
                .collect(),
            client_priorities: d.priorities.clone(),
            eligible_jobs: d.candidates().iter().map(|j| j.id).collect(),
            aging_ms: engine::AGING_MS,
            scenario: self.scenario,
            scenario_description: self.scenario.description(self.uses_datadog()),
            forecast: self.forecast.view(d.at_ms),
            at_ms: d.at_ms,
            horizon_ms: self.horizon(),
            seed: self.seed,
            paused: self.paused,
            speed: self.speed,
            policy: self.policy,
            available: self.evaluator.is_some(),
            clients: self.clients.clone(),
            nodes: d.nodes,
            jobs,
            queued,
            running,
            completed,
            rejected,
            mean_wait_ms: if waits.is_empty() {
                0.
            } else {
                waits.iter().sum::<u64>() as f64 / waits.len() as f64
            },
            oldest_wait_ms,
            history: self.history.iter().rev().take(240).cloned().rev().collect(),
            decisions: self.decisions.iter().rev().take(20).cloned().collect(),
            pending_job: self.pending.as_ref().map(|p| p.evidence.candidate.id),
            calls: self.calls,
            cost: self.cost.lock().unwrap().clone(),
            status,
            evidence_source: if self.uses_datadog() {
                "datadog"
            } else {
                "local"
            }
            .into(),
            simulation_run: self.run.clone(),
            telemetry_status: self.uses_datadog().then(|| self.telemetry_status.clone()),
            error: self.error.clone(),
        }
    }
    pub fn export(&self) -> serde_json::Value {
        serde_json::json!({"model":"reflex-scheduler-v1","state":self.data(),"view":self.view(),"decisions":self.decisions,"client_edits":self.edits,"history":self.history})
    }
    fn reset(&mut self, policy: Policy) -> Result<(), Error> {
        self.forecast.reset();
        self.forecast.local_min_samples = if self.scenario == Scenario::Cyclical {
            180
        } else {
            64
        };
        self.pending = None;
        self.policy = policy;
        if self.uses_datadog() {
            self.run = Some(datadog::new_run());
            self.clear_evidence();
        }
        self.telemetry = telemetry::Telemetry::new(self.meter.clone(), policy, self.run.clone());
        self.next_job = 1;
        self.calls = 0;
        self.last_dispatch = None;
        self.decisions.clear();
        self.history.clear();
        self.edits.clear();
        self.priority_changes.clear();
        self.error = None;
        self.paused = true;
        self.script_cursor = 0;
        if self.scenario != Scenario::Sandbox {
            self.clients = presets::clients();
            self.next_client = 3;
            if self.scenario == Scenario::Cyclical {
                self.clients = presets::cycle_clients();
            }
        }
        self.engine =
            Engine::with_priorities(self.clients.iter().map(|c| (c.id, c.priority)).collect())?;
        for c in &mut self.clients {
            c.sequence = 0;
            c.next_at = c.config.next(0);
        }
        self.refresh_telemetry();
        Ok(())
    }
    pub async fn command(&mut self, cmd: Command) -> Result<(), Error> {
        let operation = match &cmd {
            Command::Scenario { .. } => "scenario",
            Command::Forecast { .. } => "forecast",
            Command::Play => "play",
            Command::Pause => "pause",
            Command::Step => "step",
            Command::Reset => "reset",
            Command::Speed { .. } => "speed",
            Command::Policy { .. } => "policy",
            Command::AddClient => "add_client",
            Command::RemoveClient { .. } => "remove_client",
            Command::Client { .. } => "configure_client",
            Command::Priority { .. } => "priority",
        };
        if self.uses_datadog()
            && matches!(
                &cmd,
                Command::Step
                    | Command::Speed { value: 2.. }
                    | Command::Policy {
                        policy: Policy::FirstFit | Policy::BestFit
                    }
            )
        {
            return Err(Error::Invalid(
                "Datadog evidence requires Jev and continuous 1× playback".into(),
            ));
        }
        match cmd {
            Command::Scenario { scenario } => {
                self.scenario = scenario;
                self.reset(self.policy)?;
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
                if self.data().at_ms >= self.horizon() || self.error.is_some() {
                    return Err(Error::Invalid("Reset before resuming this session".into()));
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
                self.paused = true;
                self.advance(1000).await?;
                self.update_forecast().await;
                self.infer().await?;
            }
            Command::Reset => self.reset(self.policy)?,
            Command::Speed { value } => {
                if ![1, 2, 4].contains(&value) {
                    return Err(Error::Invalid("Speed must be 1, 2 or 4".into()));
                }
                self.speed = value;
            }
            Command::Policy { policy } => {
                if policy == Policy::Jev && self.evaluator.is_none() {
                    return Err(Error::Invalid("Set TYPESAFE_API_KEY to use Jev".into()));
                }
                self.reset(policy)?;
            }
            Command::AddClient => {
                if self.clients.len() >= 8 {
                    return Err(Error::Invalid("At most eight clients".into()));
                }
                let config = ClientConfig {
                    rate: 1.,
                    cpu: 2,
                    memory_gib: 4,
                    duration_ms: 6000,
                    enabled: true,
                };
                self.clients.push(Client {
                    priority: Priority::Normal,
                    id: self.next_client,
                    name: format!("Client {}", self.next_client + 1),
                    next_at: config.next(self.data().at_ms),
                    config,
                    sequence: 0,
                });
                self.next_client += 1;
                if self.uses_datadog() {
                    self.clear_evidence();
                }
            }
            Command::RemoveClient { id } => {
                if self.clients.len() == 1 {
                    return Err(Error::Invalid(
                        "Keep at least one client; pause it to stop arrivals".into(),
                    ));
                }
                let i = self
                    .clients
                    .iter()
                    .position(|c| c.id == id)
                    .ok_or_else(|| Error::Invalid("Unknown client".into()))?;
                self.clients.remove(i);
            }
            Command::Priority { id, priority } => {
                let c = self
                    .clients
                    .iter_mut()
                    .find(|c| c.id == id)
                    .ok_or_else(|| Error::Invalid("Unknown client".into()))?;
                c.priority = priority;
                self.pending = None;
                self.last_dispatch = None;
                self.engine.set_priority(id, priority).await?;
                self.priority_changes.push(PriorityChange {
                    at_ms: self.data().at_ms,
                    client: id,
                    priority,
                });
            }
            Command::Client { id, config } => {
                config.validate()?;
                let now = self.data().at_ms;
                let c = self
                    .clients
                    .iter_mut()
                    .find(|c| c.id == id)
                    .ok_or_else(|| Error::Invalid("Unknown client".into()))?;
                c.next_at = config.next(now);
                c.config = config;
            }
        }
        self.refresh_telemetry();
        tracing::info!(target: "reflex_sim::scheduler", operation, policy = telemetry::policy(self.policy), simulation_time_ms = self.data().at_ms, "Scheduler control applied");
        self.edits
            .push(serde_json::json!({"at_ms":self.data().at_ms,"clients":self.clients}));
        Ok(())
    }
    async fn update_forecast(&mut self) {
        let d = self.data();
        let window = if self.scenario == Scenario::Cyclical {
            10_000.min(d.at_ms.max(1000))
        } else {
            1000
        };
        let mut values = [0f32; 3];
        for j in d
            .jobs
            .iter()
            .filter(|j| j.arrived_at > d.at_ms.saturating_sub(window) && j.arrived_at <= d.at_ms)
        {
            values[0] += 1.;
            values[1] += j.cpu as f32 * j.duration_ms as f32 / 1000.;
            values[2] += j.memory_gib as f32 * j.duration_ms as f32 / 1000.;
        }
        for v in &mut values {
            *v /= window as f32 / 1000.;
        }
        let names = if self.scenario == Scenario::Cyclical {
            [
                "offered_jobs_per_second_10s_mean",
                "offered_cpu_seconds_per_second_10s_mean",
                "offered_gib_seconds_per_second_10s_mean",
            ]
        } else {
            [
                "offered_jobs_per_second",
                "offered_cpu_seconds_per_second",
                "offered_gib_seconds_per_second",
            ]
        };
        self.forecast.sample(d.at_ms, values, names);
        let dd = self
            .source
            .clone()
            .zip(self.run.clone())
            .map(|(s, r)| (s, r, self.not_before, "scheduler"));
        self.forecast.poll(d.at_ms, dd).await;
    }
    pub async fn tick(&mut self, ms: u64) -> Result<(), Error> {
        if !self.paused {
            self.advance(ms * self.speed as u64).await?;
            self.update_forecast().await;
            self.infer().await?;
        }
        Ok(())
    }
    async fn advance(&mut self, ms: u64) -> Result<(), Error> {
        let target = (self.data().at_ms + ms).min(self.horizon());
        loop {
            let d = self.data();
            let next = self
                .clients
                .iter()
                .filter_map(|c| c.next_at.map(|t| t.ceil() as u64))
                .chain(d.jobs.iter().filter_map(|j| j.finish_at))
                .chain(
                    self.scenario
                        .times(self.uses_datadog())
                        .get(self.script_cursor)
                        .copied(),
                )
                .filter(|t| *t <= target)
                .min();
            let Some(at) = next else { break };
            self.engine.advance(at).await?; // Completions release resources before arrivals at a tie.
            self.refresh_telemetry();
            if self
                .scenario
                .times(self.uses_datadog())
                .get(self.script_cursor)
                == Some(&at)
            {
                for c in &mut self.clients {
                    let old_rate = c.config.rate;
                    presets::update(self.scenario, self.script_cursor, c);
                    if self.scenario == Scenario::Cyclical {
                        c.next_at = if c.config.rate == 0. {
                            None
                        } else if old_rate == 0. {
                            c.config.next(at)
                        } else {
                            c.next_at.map(|next| {
                                at as f64
                                    + ((next - at as f64).max(0.) * old_rate / c.config.rate).round()
                            })
                        };
                    }
                    if self.scenario == Scenario::TrafficBurst {
                        c.next_at = c.config.next(at);
                    }
                }
                self.script_cursor += 1;
                self.edits.push(serde_json::json!({"at_ms":at,"scenario":self.scenario,"clients":self.clients,"source":"preset"}));
            }
            for i in 0..self.clients.len() {
                if self.clients[i].next_at.map(|t| t.ceil() as u64) != Some(at) {
                    continue;
                }
                let c = &mut self.clients[i];
                c.sequence += 1;
                let mut bits = self.seed ^ c.id.wrapping_mul(0x9e3779b97f4a7c15) ^ c.sequence;
                bits = (bits ^ (bits >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
                bits = (bits ^ (bits >> 27)).wrapping_mul(0x94d049bb133111eb);
                bits ^= bits >> 31;
                let actual = c.config.duration_ms * (800 + bits % 401) / 1000;
                let j = Job {
                    id: self.next_job,
                    client: c.id,
                    cpu: c.config.cpu,
                    memory_gib: c.config.memory_gib,
                    duration_ms: c.config.duration_ms,
                    actual_duration_ms: actual,
                    arrived_at: at,
                    phase: JobPhase::Queued,
                    node: None,
                    started_at: None,
                    completed_at: None,
                    finish_at: None,
                    reason: None,
                };
                self.next_job += 1;
                c.next_at = c
                    .config
                    .next_fractional(c.next_at.expect("arrival deadline"));
                self.engine.arrival(j).await?;
                self.refresh_telemetry();
            }
            self.place_baseline().await?;
        }
        self.engine.advance(target).await?;
        self.refresh_telemetry();
        self.place_baseline().await?;
        if self.history.last().is_none_or(|s| target >= s.at_ms + 250) {
            let d = self.data();
            self.history.push(Sample {
                nodes: d.nodes.clone(),
                clients: self
                    .telemetry_clients()
                    .iter()
                    .map(|id| d.client_lag(*id))
                    .collect(),
                at_ms: target,
                queued: d
                    .jobs
                    .iter()
                    .filter(|j| j.phase == JobPhase::Queued)
                    .count(),
                running: d
                    .jobs
                    .iter()
                    .filter(|j| j.phase == JobPhase::Running)
                    .count(),
                completed: d
                    .jobs
                    .iter()
                    .filter(|j| j.phase == JobPhase::Completed)
                    .count(),
            });
        }
        if target == self.horizon() {
            self.paused = true;
            if self.uses_datadog() {
                self.clear_evidence();
            }
        }
        Ok(())
    }
    async fn place_baseline(&mut self) -> Result<(), Error> {
        if self.policy == Policy::Jev {
            return Ok(());
        }
        while let Some(e) = judge::evidence(&self.data()) {
            let choice = if self.policy == Policy::FirstFit {
                e.legal_choices[0]
            } else {
                e.legal_choices
                    .iter()
                    .filter(|c| c.job(e.candidate.id) == e.candidate.id)
                    .filter_map(|c| c.node().map(|i| (*c, &e.nodes[i])))
                    .min_by(|(_, a), (_, b)| {
                        let score = |n: &judge::NodeEvidence| {
                            (n.available_cpu - e.candidate.cpu) as f64 / n.total_cpu as f64
                                + (n.available_memory_gib - e.candidate.memory_gib) as f64
                                    / n.total_memory_gib as f64
                        };
                        score(a).total_cmp(&score(b))
                    })
                    .unwrap()
                    .0
            };
            self.apply(e, Some(choice), None).await?;
        }
        Ok(())
    }
    async fn apply(
        &mut self,
        e: Evidence,
        choice: Option<Choice>,
        result: Option<judge::Inference>,
    ) -> Result<(), Error> {
        let selected = e
            .candidates
            .iter()
            .find(|j| {
                j.id == choice
                    .map(|c| c.job(e.candidate.id))
                    .unwrap_or(e.candidate.id)
            })
            .unwrap_or(&e.candidate);
        let mut trace = DecisionTrace::scheduler(
            selected.id,
            selected.client,
            telemetry::policy(self.policy),
            self.data().at_ms,
        );
        self.apply_traced(&mut trace, e, choice, result).await
    }
    async fn apply_traced(
        &mut self,
        trace: &mut DecisionTrace,
        e: Evidence,
        choice: Option<Choice>,
        result: Option<judge::Inference>,
    ) -> Result<(), Error> {
        if let Some(selected) = e.candidates.iter().find(|j| {
            j.id == choice
                .map(|c| c.job(e.candidate.id))
                .unwrap_or(e.candidate.id)
        }) {
            trace.scheduler_selection(selected.id, selected.client);
        }
        let span = trace.child("apply");
        let applied = trace
            .scope(self.apply_inner(e, choice, result).instrument(span))
            .await;
        trace.finish(if applied.is_err() {
            "error"
        } else {
            self.decisions
                .last()
                .expect("application records decision")
                .status
                .as_str()
        });
        applied
    }
    async fn apply_inner(
        &mut self,
        e: Evidence,
        choice: Option<Choice>,
        result: Option<judge::Inference>,
    ) -> Result<(), Error> {
        let selected = e
            .candidates
            .iter()
            .find(|j| {
                j.id == choice
                    .map(|c| c.job(e.candidate.id))
                    .unwrap_or(e.candidate.id)
            })
            .unwrap_or(&e.candidate)
            .clone();
        let invalid_choice = choice.is_some_and(|c| !e.legal_choices.contains(&c));
        let (status, reason): (String, String) = if invalid_choice {
            (
                "rejected".into(),
                "Choice was not offered by this evaluation".into(),
            )
        } else if let Some(c) = choice {
            let (ok, reason) = self
                .engine
                .apply(
                    Placement {
                        job: selected.id,
                        priority_revision: e.priority_revision,
                        choice: c,
                        observed_at: e.observed_at_ms,
                    },
                    result.as_ref().and_then(|r| r.confidence),
                )
                .await?;
            (
                if ok {
                    if c == Choice::Defer {
                        "deferred"
                    } else {
                        "placed"
                    }
                } else {
                    "rejected"
                }
                .into(),
                reason,
            )
        } else {
            self.engine
                .evaluation_error(
                    result
                        .as_ref()
                        .and_then(|r| r.error.as_deref())
                        .unwrap_or("No choice supplied"),
                )
                .await?;
            (
                "evaluation_error".into(),
                result
                    .as_ref()
                    .and_then(|r| r.error.clone())
                    .unwrap_or_else(|| "No choice supplied".into()),
            )
        };
        self.refresh_telemetry();
        self.telemetry
            .placement(selected.client, choice.and_then(Choice::node), &status);
        tracing::info!(target: "reflex_sim::scheduler", job_id = selected.id, client = telemetry::client(selected.client),
            policy = telemetry::policy(self.policy), outcome = status.as_str(), node = choice.and_then(Choice::node).map(telemetry::node), "Scheduler placement evaluated");
        self.decisions.push(Decision {
            at_ms: self.data().at_ms,
            job: selected.id,
            policy: self.policy,
            choice,
            status,
            reason,
            evidence: if result.is_some() { Some(e) } else { None },
            result,
        });
        Ok(())
    }
    async fn infer(&mut self) -> Result<(), Error> {
        if self.uses_datadog() && (self.paused || self.data().at_ms >= self.horizon()) {
            return Ok(());
        }
        if self.policy != Policy::Jev {
            return Ok(());
        }
        if self.uses_datadog() {
            self.poll_evidence().await;
        }
        if self.pending.as_ref().is_some_and(|p| p.task.is_finished()) {
            let mut p = self.pending.take().unwrap();
            let r = match (&mut p.task).await {
                Ok(r) => r,
                Err(_) => judge::Inference::failed("Inference task failed"),
            };
            self.apply_traced(&mut p.trace, p.evidence.clone(), r.choice, Some(r))
                .await?;
        }
        if self.pending.is_some() || self.data().at_ms >= self.horizon() {
            return Ok(());
        }
        let Some(mut e) = judge::evidence(&self.data()) else {
            return Ok(());
        };
        if self.uses_datadog() {
            if let Some(telemetry) = self.cached.clone() {
                let valid = telemetry
                    .validate(
                        self.run.as_deref().unwrap(),
                        self.not_before,
                        crate::datadog::unix_ms(),
                    )
                    .and_then(|()| {
                        if self.telemetry_clients().iter().any(|id| {
                            !telemetry
                                .queues
                                .iter()
                                .any(|q| q.client == telemetry::client(*id))
                        }) {
                            Err("Incomplete client telemetry".into())
                        } else {
                            Ok(())
                        }
                    });
                match valid {
                    Ok(()) => {
                        self.telemetry_status = format!(
                            "Current placement state + Datadog context ({:.0}s old)",
                            (crate::datadog::unix_ms() - telemetry.observed_at_unix_ms) as f64
                                / 1000.
                        );
                        e.telemetry = Some(telemetry);
                    }
                    Err(reason) => {
                        self.cached = None;
                        self.telemetry_status = format!("Scheduling from current state; {reason}");
                    }
                }
            }
        }
        // Aging can leave exactly one legal placement. No heuristic is needed, and
        // the TypeSafe choice task requires at least two alternatives.
        if e.legal_choices.len() == 1 {
            let choice = e.legal_choices[0];
            self.apply(e, Some(choice), None).await?;
            return Ok(());
        }
        if self
            .last_dispatch
            .is_some_and(|t| t.elapsed() < self.settings.dispatch_interval)
        {
            return Ok(());
        }
        let Some(evaluator) = self.evaluator.clone() else {
            return Ok(());
        };
        e.forecast = self.forecast.evidence(self.data().at_ms);
        let captured = e.clone();
        let cost = self.cost.clone();
        cost.lock().unwrap().dispatched();
        self.calls += 1;
        self.last_dispatch = Some(Instant::now());
        let trace = DecisionTrace::scheduler(
            e.candidate.id,
            e.candidate.client,
            telemetry::policy(self.policy),
            self.data().at_ms,
        );
        let span = trace.child("evaluate");
        let task = tokio::spawn(
            trace.scope(
                async move {
                    let r = evaluator.evaluate(captured).await;
                    cost.lock()
                        .unwrap()
                        .record_usage(r.model.as_deref(), r.usage.as_ref());
                    r
                }
                .instrument(span),
            ),
        );
        self.pending = Some(Pending {
            evidence: e,
            task,
            trace,
        });
        Ok(())
    }
}

#[cfg(test)]
use crate::metric_capture;
#[cfg(test)]
mod datadog_tests {
    use super::*;
    struct Judge(Arc<Mutex<Vec<serde_json::Value>>>);
    impl Evaluator for Judge {
        fn evaluate(&self, e: Evidence) -> judge::Evaluation<'_> {
            self.0
                .lock()
                .unwrap()
                .push(serde_json::to_value(e).unwrap());
            Box::pin(async {
                judge::Inference {
                    choice: Some(Choice::NodeA),
                    confidence: Some(0.9),
                    probabilities: Default::default(),
                    model: None,
                    usage: None,
                    latency_ms: 0.,
                    error: None,
                }
            })
        }
    }
    fn remote(s: &Session) -> datadog::TelemetryEvidence {
        datadog::TelemetryEvidence {
            source: "datadog".into(),
            simulation_run: s.run.clone().unwrap(),
            fetched_at_unix_ms: crate::datadog::unix_ms(),
            observed_at_unix_ms: crate::datadog::unix_ms() - 30_000,
            nodes: s
                .data()
                .nodes
                .iter()
                .enumerate()
                .map(|(i, n)| datadog::NodeEvidence {
                    node: telemetry::node(i).into(),
                    cpu_capacity: n.cpu as f64,
                    cpu_reserved: 0.,
                    memory_capacity_bytes: n.memory_gib as f64 * 1073741824.,
                    memory_reserved_bytes: 0.,
                    running_jobs: 0.,
                })
                .collect(),
            queues: (0..3)
                .map(|i| datadog::QueueEvidence {
                    client: telemetry::client(i),
                    queued_jobs: 10.,
                    oldest_age_seconds: 20.,
                })
                .collect(),
        }
    }
    #[tokio::test]
    async fn datadog_metrics_are_run_scoped_and_removed_clients_export_zero() {
        use opentelemetry::metrics::MeterProvider;
        let capture = metric_capture::Capture::new();
        let evaluator = Arc::new(datadog::DatadogEvaluator::new(
            Arc::new(Judge(Arc::new(Mutex::new(vec![])))),
            crate::datadog::Source::for_test("http://127.0.0.1:1"),
        ));
        let mut s = Session::with_meter(
            42,
            Some(evaluator),
            JevSettings::default(),
            capture.provider.meter("scheduler"),
        )
        .unwrap();
        let run = s.run.clone().unwrap();
        s.advance(4000).await.unwrap();
        let mut input = judge::evidence(&s.data()).unwrap();
        s.not_before = crate::datadog::unix_ms() - 60_000;
        input.telemetry = Some(remote(&s));
        s.apply(input, Some(Choice::NodeA), None).await.unwrap();
        s.advance(10_000).await.unwrap();
        let m = capture.read();
        assert!(metric_capture::count(&m, "scheduler.jobs", &[("simulation_run", &run)]) > 0);
        assert_eq!(
            metric_capture::count(&m, "scheduler.placements", &[("simulation_run", &run)]),
            1
        );
        assert!(
            metric_capture::histogram(&m, "scheduler.job.cpu", &[("simulation_run", &run)]).0 > 0
        );
        assert_eq!(
            metric_capture::gauge(
                &m,
                "scheduler.node.cpu.capacity",
                &[("simulation_run", &run)]
            )
            .len(),
            4
        );
        s.command(Command::Reset).await.unwrap();
        let new_run = s.run.clone().unwrap();
        assert_ne!(run, new_run);
        s.command(Command::RemoveClient { id: 2 }).await.unwrap();
        let m = capture.read();
        assert!(metric_capture::gauge(
            &m,
            "scheduler.node.cpu.capacity",
            &[("simulation_run", &run)]
        )
        .is_empty());
        assert_eq!(
            metric_capture::gauge(
                &m,
                "scheduler.queue.depth",
                &[("simulation_run", &new_run), ("client", "client_3")]
            ),
            vec![0.]
        );
    }
    #[tokio::test]
    async fn placement_continues_with_missing_stale_or_incomplete_telemetry() {
        for context in ["missing", "stale", "incomplete", "valid", "fetch_failed"] {
            let captured = Arc::new(Mutex::new(vec![]));
            let evaluator = Arc::new(datadog::DatadogEvaluator::new(
                Arc::new(Judge(captured.clone())),
                crate::datadog::Source::for_test("http://127.0.0.1:1"),
            ));
            let mut s = Session::new(42, Some(evaluator), JevSettings::default()).unwrap();
            s.command(Command::Play).await.unwrap();
            s.advance(4000).await.unwrap();
            s.last_fetch = Some(Instant::now());
            s.not_before = crate::datadog::unix_ms() - 60_000;
            if context != "missing" {
                let mut t = remote(&s);
                if context == "stale" {
                    t.observed_at_unix_ms -= 60_000;
                }
                if context == "incomplete" {
                    t.queues.pop();
                }
                s.cached = Some(t);
            }
            if context == "fetch_failed" {
                s.fetch = Some(Fetch {
                    task: tokio::spawn(async { Err("test refresh failure".into()) }),
                });
                while !s.fetch.as_ref().unwrap().task.is_finished() {
                    tokio::task::yield_now().await;
                }
            }
            s.infer().await.unwrap();
            assert_eq!(s.calls, 1, "{context}");
            while !s.pending.as_ref().unwrap().task.is_finished() {
                tokio::task::yield_now().await;
            }
            // Expiring telemetry after dispatch must not invalidate a legal placement.
            if let Some(t) = &mut s.pending.as_mut().unwrap().evidence.telemetry {
                t.observed_at_unix_ms -= 60_000;
            }
            s.cached = None;
            s.infer().await.unwrap();
            assert_eq!(s.view().running, 1, "{context}");
            let input = captured.lock().unwrap()[0].clone();
            assert_eq!(input["nodes"].as_array().unwrap().len(), 4);
            assert!(input["waiting_jobs"].as_u64().unwrap() > 0);
            assert!(input["candidate"]["id"].is_number());
            assert_eq!(
                input.get("telemetry").is_some(),
                matches!(context, "valid" | "fetch_failed")
            );
            // Reusing an already-placed job is still rejected by the local executor.
            let mut e = s.decisions.last().unwrap().evidence.clone().unwrap();
            if let Some(t) = &mut e.telemetry {
                t.observed_at_unix_ms -= 60_000;
            }
            let before = serde_json::to_value(s.data()).unwrap();
            s.apply(e, Some(Choice::NodeA), None).await.unwrap();
            assert_eq!(before, serde_json::to_value(s.data()).unwrap());
            assert_eq!(s.decisions.last().unwrap().status, "rejected");
            s.command(Command::Pause).await.unwrap();
            assert!(s.cached.is_none());
            assert!(s.pending.is_none());
            assert!(s.fetch.is_none());
        }
    }
}
