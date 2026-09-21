use super::workload::{Arrival, Bucket};
use crate::Error;
use reflex::{
    state_machine, ExecutionOutcome, InMemory, Judgment, Rejection, StateMachineExecutor,
};
use serde::{Deserialize, Serialize};
use std::time::Instant;
pub const NODE_CPU: u32 = 8;
pub const NODE_MEMORY: u32 = 16;
pub const NODES: usize = 6;
pub const HORIZON_MS: u64 = 600_000;
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Lifecycle {
    Off,
    Starting,
    Ready,
    Draining,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Node {
    pub id: usize,
    pub phase: Lifecycle,
    pub ready_at: Option<u64>,
    pub cpu: u32,
    pub memory_gib: u32,
    pub used_cpu: u32,
    pub used_memory_gib: u32,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobPhase {
    Queued,
    Running,
    Completed,
    Rejected,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Job {
    pub arrival: Arrival,
    pub phase: JobPhase,
    pub arrived_at: u64,
    pub started_at: Option<u64>,
    pub finish_at: Option<u64>,
    pub node: Option<usize>,
    pub reason: Option<String>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Hold,
    StartOne,
    StartTwo,
    DrainOne,
}
impl Action {
    pub fn label(self) -> &'static str {
        match self {
            Self::Hold => "Hold capacity",
            Self::StartOne => "Start one node",
            Self::StartTwo => "Start two nodes",
            Self::DrainOne => "Drain one node",
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Settings {
    pub startup_s: u64,
    pub max_nodes: usize,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            startup_s: 60,
            max_nodes: 6,
        }
    }
}
impl Settings {
    pub fn valid(&self) -> bool {
        (5..=120).contains(&self.startup_s) && (2..=NODES).contains(&self.max_nodes)
    }
}
#[derive(Clone, Debug, Serialize)]
pub struct Change {
    pub at_ms: u64,
    pub action: Action,
}
#[derive(Clone, Debug, Serialize)]
pub struct Data {
    pub at_ms: u64,
    pub revision: u64,
    pub nodes: Vec<Node>,
    pub jobs: Vec<Job>,
    pub settings: Settings,
    pub last_action: Option<u64>,
    pub changes: Vec<Change>,
    pub node_seconds: u64,
    pub offered: u64,
    pub wait_sum_ms: u64,
    pub starts: u64,
}
impl Data {
    pub fn queued(&self) -> usize {
        self.jobs
            .iter()
            .filter(|j| j.phase == JobPhase::Queued)
            .count()
    }
    pub fn running(&self) -> usize {
        self.jobs
            .iter()
            .filter(|j| j.phase == JobPhase::Running)
            .count()
    }
    pub fn count(&self, p: Lifecycle) -> usize {
        self.nodes.iter().filter(|n| n.phase == p).count()
    }
    pub fn active(&self) -> usize {
        NODES - self.count(Lifecycle::Off)
    }
    pub fn mean_wait(&self) -> f64 {
        self.wait_sum_ms as f64 / self.starts.max(1) as f64
    }
}
#[derive(Clone)]
pub struct Proposal {
    pub action: Action,
    pub observed_at: u64,
    pub revision: u64,
    pub forecast_origin: Option<u64>,
}
#[derive(Clone)]
pub enum Event {
    Tick(Bucket),
    Configure(Settings),
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Managing,
}
fn reject(code: &str, msg: &str) -> Rejection {
    Rejection::new(code, msg)
}
pub fn legal(d: &Data, a: Action) -> Result<(), Rejection> {
    if a == Action::Hold {
        return Ok(());
    }
    if d.last_action.is_some_and(|t| d.at_ms < t + 10_000) {
        return Err(reject("cooldown", "Ten seconds between capacity changes"));
    }
    match a {
        Action::StartOne | Action::StartTwo => {
            let n = if a == Action::StartOne { 1 } else { 2 };
            if d.active() + n > d.settings.max_nodes {
                return Err(reject(
                    "budget",
                    "Starting and draining nodes count toward the node budget",
                ));
            }
        }
        Action::DrainOne => {
            if d.count(Lifecycle::Ready) <= 1 || d.count(Lifecycle::Draining) > 0 || d.queued() > 0
            {
                return Err(reject(
                    "drain",
                    "Retain one ready node, clear the queue and finish any existing drain",
                ));
            }
        }
        Action::Hold => {}
    }
    Ok(())
}
pub fn actions(d: &Data) -> Vec<Action> {
    [
        Action::Hold,
        Action::StartOne,
        Action::StartTwo,
        Action::DrainOne,
    ]
    .into_iter()
    .filter(|a| legal(d, *a).is_ok())
    .collect()
}
fn guard(d: &Data, p: &Proposal, _: Instant) -> Result<(), Rejection> {
    if p.observed_at > d.at_ms || d.at_ms - p.observed_at > 10_000 {
        return Err(reject("stale", "Decision evidence expired"));
    }
    if p.revision != d.revision {
        return Err(reject(
            "changed",
            "Capacity lifecycle or configuration changed",
        ));
    }
    if p.forecast_origin
        .is_some_and(|t| t > d.at_ms || d.at_ms - t > super::forecast::MAX_AGE_MS)
    {
        return Err(reject("forecast_expired", "Forecast is no longer fresh"));
    }
    legal(d, p.action)
}
fn apply(d: &mut Data, p: &Proposal, _: Instant) -> Result<(), Rejection> {
    match p.action {
        Action::Hold => return Ok(()),
        Action::StartOne | Action::StartTwo => {
            let count = if p.action == Action::StartOne { 1 } else { 2 };
            for n in d
                .nodes
                .iter_mut()
                .filter(|n| n.phase == Lifecycle::Off)
                .take(count)
            {
                n.phase = Lifecycle::Starting;
                n.ready_at = Some(d.at_ms + d.settings.startup_s * 1000);
            }
        }
        Action::DrainOne => {
            let n = d
                .nodes
                .iter_mut()
                .filter(|n| n.phase == Lifecycle::Ready)
                .min_by_key(|n| (n.used_cpu, n.used_memory_gib, n.id))
                .unwrap();
            n.phase = if n.used_cpu == 0 {
                Lifecycle::Off
            } else {
                Lifecycle::Draining
            };
        }
    }
    d.last_action = Some(d.at_ms);
    d.revision += 1;
    d.changes.push(Change {
        at_ms: d.at_ms,
        action: p.action,
    });
    Ok(())
}
pub fn invariant(_: &Phase, d: &Data) -> Result<(), Rejection> {
    if !d.settings.valid()
        || d.nodes.len() != NODES
        || d.active() > d.settings.max_nodes
        || d.count(Lifecycle::Ready) == 0
    {
        return Err(reject("capacity", "Invalid node budget or no ready node"));
    }
    for n in &d.nodes {
        let jobs: Vec<_> = d
            .jobs
            .iter()
            .filter(|j| j.phase == JobPhase::Running && j.node == Some(n.id))
            .collect();
        let cpu: u64 = jobs.iter().map(|j| j.arrival.cpu as u64).sum();
        let mem: u64 = jobs.iter().map(|j| j.arrival.memory_gib as u64).sum();
        if cpu != n.used_cpu as u64
            || mem != n.used_memory_gib as u64
            || cpu > n.cpu as u64
            || mem > n.memory_gib as u64
            || (!matches!(n.phase, Lifecycle::Ready | Lifecycle::Draining) && !jobs.is_empty())
            || (n.phase == Lifecycle::Starting) != n.ready_at.is_some()
        {
            return Err(reject(
                "reservation",
                "Reservations must match running jobs and fit actual ready/draining capacity",
            ));
        }
    }
    if d.queued() > 128 || d.offered != d.jobs.len() as u64 {
        return Err(reject(
            "accounting",
            "Bounded queues must conserve offered jobs",
        ));
    }
    let mut ids = std::collections::HashSet::new();
    if d.jobs.iter().any(|j| {
        !ids.insert(j.arrival.id)
            || j.arrival.actual_s == 0
            || j.arrival.cpu == 0
            || j.arrival.memory_gib == 0
            || j.phase == JobPhase::Running
                && (j.node.is_none_or(|n| n >= NODES)
                    || j.finish_at.is_none()
                    || j.started_at.is_none())
            || j.phase != JobPhase::Running && j.finish_at.is_some()
    }) {
        return Err(reject("job", "Invalid job lifecycle"));
    }
    Ok(())
}
fn tick(d: &mut Data, b: &Bucket) -> Result<(), Rejection> {
    if b.at_ms != d.at_ms as i64 + 1000 {
        return Err(reject(
            "clock",
            "Capacity simulation advances in one-second ticks",
        ));
    }
    d.node_seconds += d.active() as u64;
    d.at_ms = b.at_ms as u64;
    for n in &mut d.nodes {
        if n.phase == Lifecycle::Starting && n.ready_at.is_some_and(|t| t <= d.at_ms) {
            n.phase = Lifecycle::Ready;
            n.ready_at = None;
            d.revision += 1;
        }
    }
    for j in &mut d.jobs {
        if j.phase == JobPhase::Running && j.finish_at.is_some_and(|t| t <= d.at_ms) {
            let n = &mut d.nodes[j.node.unwrap()];
            n.used_cpu -= j.arrival.cpu;
            n.used_memory_gib -= j.arrival.memory_gib;
            j.phase = JobPhase::Completed;
            j.finish_at = None;
        }
        if j.phase == JobPhase::Queued && d.at_ms - j.arrived_at >= 60_000 {
            j.phase = JobPhase::Rejected;
            j.reason = Some("Queue deadline exceeded (60s)".into());
        }
    }
    for n in &mut d.nodes {
        if n.phase == Lifecycle::Draining && n.used_cpu == 0 {
            n.phase = Lifecycle::Off;
            d.revision += 1;
        }
    }
    for a in &b.jobs {
        let reason = if a.cpu > NODE_CPU || a.memory_gib > NODE_MEMORY {
            Some("Request is larger than a node")
        } else if d.queued() >= 128 {
            Some("Queue full (128 jobs)")
        } else {
            None
        };
        d.jobs.push(Job {
            arrival: a.clone(),
            phase: if reason.is_some() {
                JobPhase::Rejected
            } else {
                JobPhase::Queued
            },
            arrived_at: d.at_ms,
            started_at: None,
            finish_at: None,
            node: None,
            reason: reason.map(str::to_string),
        });
        d.offered += 1;
    }
    // Identical deterministic FIFO placement in every lane isolates capacity policy effects.
    loop {
        let Some(i) = d.jobs.iter().position(|j| j.phase == JobPhase::Queued) else {
            break;
        };
        let a = &d.jobs[i].arrival;
        let Some(n) = d.nodes.iter_mut().find(|n| {
            n.phase == Lifecycle::Ready
                && n.cpu - n.used_cpu >= a.cpu
                && n.memory_gib - n.used_memory_gib >= a.memory_gib
        }) else {
            break;
        };
        n.used_cpu += a.cpu;
        n.used_memory_gib += a.memory_gib;
        let j = &mut d.jobs[i];
        j.phase = JobPhase::Running;
        j.node = Some(n.id);
        j.started_at = Some(d.at_ms);
        j.finish_at = Some(d.at_ms + j.arrival.actual_s * 1000);
        d.wait_sum_ms += d.at_ms - j.arrived_at;
        d.starts += 1;
    }
    Ok(())
}
fn event(d: &mut Data, e: &Event, _: Instant) -> Result<(), Rejection> {
    match e {
        Event::Tick(b) => tick(d, b),
        Event::Configure(s) => {
            if !s.valid() || s.max_nodes < d.active() {
                return Err(reject(
                    "settings",
                    "Use 5–120s startup and a node budget at least as large as active capacity",
                ));
            }
            d.settings = s.clone();
            d.revision += 1;
            Ok(())
        }
    }
}
pub struct Engine {
    machine: StateMachineExecutor<Phase, Data, Proposal, Event>,
}
impl Engine {
    pub fn new(settings: Settings) -> Result<Self, Error> {
        let definition = state_machine! {phase:Phase,data:Data,action:Proposal,event:Event,invariants:[invariant],transitions:[Phase::Managing+action(_)=>Phase::Managing{guard:guard,update:apply},Phase::Managing+event(_)=>Phase::Managing{update:event},Phase::Managing+evaluation_error(_)=>unchanged{}]};
        let nodes = (0..NODES)
            .map(|id| Node {
                id,
                phase: if id < 2 {
                    Lifecycle::Ready
                } else {
                    Lifecycle::Off
                },
                ready_at: None,
                cpu: NODE_CPU,
                memory_gib: NODE_MEMORY,
                used_cpu: 0,
                used_memory_gib: 0,
            })
            .collect();
        let data = Data {
            at_ms: 0,
            revision: 0,
            nodes,
            jobs: vec![],
            settings,
            last_action: None,
            changes: vec![],
            node_seconds: 0,
            offered: 0,
            wait_sum_ms: 0,
            starts: 0,
        };
        Ok(Self {
            machine: StateMachineExecutor::builder(definition)
                .store(InMemory::new(Phase::Managing, data))
                .build()
                .map_err(|e| Error::Invalid(e.to_string()))?,
        })
    }
    pub fn data(&self) -> Data {
        self.machine.state().expect("in-memory state").1
    }
    pub async fn event(&mut self, e: Event) -> Result<(), Error> {
        match self
            .machine
            .handle_event(e)
            .await
            .map_err(|e| Error::Policy(e.to_string()))?
        {
            ExecutionOutcome::Applied(_) => Ok(()),
            ExecutionOutcome::Rejected { reason, .. } => Err(Error::Invalid(reason.message)),
        }
    }
    pub async fn apply(
        &mut self,
        p: Proposal,
        confidence: Option<f64>,
    ) -> Result<(bool, String), Error> {
        Ok(
            match self
                .machine
                .execute(
                    Judgment {
                        action: p,
                        confidence,
                    }
                    .try_into(),
                )
                .await
                .map_err(|e| Error::Policy(e.to_string()))?
            {
                ExecutionOutcome::Applied(r) if r.evaluation_error.is_some() => {
                    (false, "Invalid judgment; capacity retained".into())
                }
                ExecutionOutcome::Applied(_) => (
                    true,
                    "Freshness, lifecycle, budget and draining guards passed".into(),
                ),
                ExecutionOutcome::Rejected { reason, .. } => {
                    (false, format!("{}: {}", reason.code, reason.message))
                }
            },
        )
    }
    pub async fn evaluation_error(&mut self, message: &str) -> Result<(), Error> {
        self.machine
            .execute(Err(reflex::EvaluationError::Judge(
                reflex::JudgeError::new("capacity_inference", message),
            )))
            .await
            .map_err(|e| Error::Policy(e.to_string()))?;
        Ok(())
    }
}
