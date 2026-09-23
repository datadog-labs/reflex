// Unless explicitly stated otherwise all files in this repository are licensed under the Apache-2.0 License.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-present Datadog, Inc.

use crate::Error;
use reflex::{
    state_machine, ExecutionOutcome, InMemory, Judgment, Rejection, StateMachineExecutor,
};
use serde::{Deserialize, Serialize};
use std::time::Instant;

pub const HORIZON_MS: u64 = 180_000;
pub const QUEUE_LIMIT: usize = 128;
pub const AGING_MS: u64 = 30_000;
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Priority {
    #[default]
    Normal,
    High,
    Critical,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobPhase {
    Queued,
    Running,
    Completed,
    Rejected,
}
#[derive(Debug, Clone, Serialize)]
pub struct Job {
    pub id: u64,
    pub client: u64,
    pub cpu: u32,
    pub memory_gib: u32,
    pub duration_ms: u64,
    pub arrived_at: u64,
    pub phase: JobPhase,
    pub node: Option<usize>,
    pub started_at: Option<u64>,
    pub completed_at: Option<u64>,
    pub reason: Option<String>,
    #[serde(skip_serializing)]
    pub finish_at: Option<u64>,
    #[serde(skip_serializing)]
    pub actual_duration_ms: u64,
}
#[derive(Debug, Clone, Serialize)]
pub struct Node {
    pub name: String,
    pub cpu: u32,
    pub memory_gib: u32,
    pub used_cpu: u32,
    pub used_memory_gib: u32,
}
impl Node {
    pub fn fits(&self, j: &Job) -> bool {
        j.cpu <= self.cpu - self.used_cpu && j.memory_gib <= self.memory_gib - self.used_memory_gib
    }
}
#[derive(Debug, Clone, Serialize)]
pub struct Data {
    pub priorities: std::collections::BTreeMap<u64, Priority>,
    pub priority_revision: u64,
    pub at_ms: u64,
    pub revision: u64,
    pub jobs: Vec<Job>,
    pub nodes: Vec<Node>,
}
#[derive(Clone, Serialize)]
pub struct ClientStats {
    pub client: u64,
    pub priority: Priority,
    pub queued: usize,
    pub completed: usize,
    pub completed_per_second: f64,
    pub started: usize,
    pub mean_wait_ms: Option<f64>,
    pub p95_wait_ms: Option<u64>,
    pub oldest_wait_ms: u64,
}
#[derive(Clone, Serialize)]
pub struct ClientLag {
    pub client: u64,
    pub priority: Priority,
    pub queued: usize,
    pub oldest_wait_ms: u64,
    pub recent_mean_start_lag_ms: Option<f64>,
    pub recent_starts: usize,
}
impl Data {
    pub fn client_lag(&self, client: u64) -> ClientLag {
        let queued: Vec<_> = self
            .jobs
            .iter()
            .filter(|j| j.client == client && j.phase == JobPhase::Queued)
            .collect();
        let waits: Vec<_> = self
            .jobs
            .iter()
            .filter(|j| j.client == client)
            .filter_map(|j| {
                j.started_at
                    .filter(|at| *at <= self.at_ms && self.at_ms - *at < 10_000)
                    .map(|at| at - j.arrived_at)
            })
            .collect();
        ClientLag {
            client,
            priority: self.priority(client),
            queued: queued.len(),
            oldest_wait_ms: queued
                .iter()
                .map(|j| self.at_ms - j.arrived_at)
                .max()
                .unwrap_or(0),
            recent_mean_start_lag_ms: (!waits.is_empty())
                .then(|| waits.iter().sum::<u64>() as f64 / waits.len() as f64),
            recent_starts: waits.len(),
        }
    }

    pub fn client_stats(&self, client: u64) -> ClientStats {
        let jobs: Vec<_> = self.jobs.iter().filter(|j| j.client == client).collect();
        let mut waits: Vec<_> = jobs
            .iter()
            .filter_map(|j| j.started_at.map(|at| at - j.arrived_at))
            .collect();
        waits.sort_unstable();
        let completed = jobs
            .iter()
            .filter(|j| j.phase == JobPhase::Completed)
            .count();
        ClientStats {
            client,
            priority: self.priority(client),
            queued: jobs.iter().filter(|j| j.phase == JobPhase::Queued).count(),
            completed,
            completed_per_second: if self.at_ms == 0 {
                0.
            } else {
                completed as f64 * 1000. / self.at_ms as f64
            },
            started: waits.len(),
            mean_wait_ms: (!waits.is_empty())
                .then(|| waits.iter().sum::<u64>() as f64 / waits.len() as f64),
            p95_wait_ms: (!waits.is_empty()).then(|| waits[(waits.len() * 95).div_ceil(100) - 1]),
            oldest_wait_ms: jobs
                .iter()
                .filter(|j| j.phase == JobPhase::Queued)
                .map(|j| self.at_ms - j.arrived_at)
                .max()
                .unwrap_or(0),
        }
    }

    pub fn priority(&self, client: u64) -> Priority {
        self.priorities.get(&client).copied().unwrap_or_default()
    }
    /// One head per client; blocked heads never expose younger requests from that client.
    pub fn heads(&self) -> Vec<&Job> {
        let mut seen = std::collections::HashSet::new();
        self.jobs
            .iter()
            .filter(|j| j.phase == JobPhase::Queued && seen.insert(j.client))
            .collect()
    }
    pub fn overdue_head(&self) -> Option<&Job> {
        self.heads()
            .into_iter()
            .filter(|j| {
                self.at_ms.saturating_sub(j.arrived_at) >= AGING_MS
                    && self.nodes.iter().any(|n| n.fits(j))
            })
            .min_by_key(|j| (j.arrived_at, j.id))
    }
    pub fn candidates(&self) -> Vec<&Job> {
        if let Some(j) = self.overdue_head() {
            return vec![j];
        }
        let mut heads: Vec<_> = self
            .heads()
            .into_iter()
            .filter(|j| self.nodes.iter().any(|n| n.fits(j)))
            .collect();
        heads.sort_by_key(|j| {
            (
                std::cmp::Reverse(self.priority(j.client)),
                j.arrived_at,
                j.id,
            )
        });
        heads
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Scheduling,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Choice {
    NodeA,
    NodeB,
    NodeC,
    NodeD,
    Defer,
    Place { job: u64, node: usize },
}
impl Choice {
    pub fn node(self) -> Option<usize> {
        match self {
            Self::NodeA => Some(0),
            Self::NodeB => Some(1),
            Self::NodeC => Some(2),
            Self::NodeD => Some(3),
            Self::Defer => None,
            Self::Place { node, .. } => Some(node),
        }
    }
    pub fn job(self, default: u64) -> u64 {
        match self {
            Self::Place { job, .. } => job,
            _ => default,
        }
    }
    pub fn for_node(n: usize) -> Self {
        [Self::NodeA, Self::NodeB, Self::NodeC, Self::NodeD][n]
    }
}
#[derive(Clone)]
pub struct Placement {
    pub priority_revision: u64,
    pub job: u64,
    pub choice: Choice,
    pub observed_at: u64,
}
#[derive(Clone)]
enum Event {
    Arrive(Job),
    Clock(u64),
    Priority { client: u64, priority: Priority },
}
pub fn invariant(_: &Phase, d: &Data) -> Result<(), Rejection> {
    for (i, n) in d.nodes.iter().enumerate() {
        let running: Vec<_> = d
            .jobs
            .iter()
            .filter(|j| j.phase == JobPhase::Running && j.node == Some(i))
            .collect();
        let cpu: u64 = running.iter().map(|j| j.cpu as u64).sum();
        let mem: u64 = running.iter().map(|j| j.memory_gib as u64).sum();
        if cpu != n.used_cpu as u64
            || mem != n.used_memory_gib as u64
            || cpu > n.cpu as u64
            || mem > n.memory_gib as u64
        {
            return Err(Rejection::new(
                "capacity",
                "Reservations must equal running work and fit node capacity",
            ));
        }
    }
    let mut ids = std::collections::HashSet::new();
    for j in &d.jobs {
        if !ids.insert(j.id)
            || j.duration_ms == 0
            || j.actual_duration_ms == 0
            || j.cpu == 0
            || j.memory_gib == 0
            || (j.phase == JobPhase::Running
                && (j.node.is_none_or(|n| n >= d.nodes.len())
                    || j.finish_at.is_none()
                    || j.started_at.is_none()))
            || (j.phase != JobPhase::Running && j.finish_at.is_some())
            || (j.phase == JobPhase::Queued && (j.node.is_some() || j.started_at.is_some()))
        {
            return Err(Rejection::new(
                "job_assignment",
                "Jobs must have unique IDs and valid reservations",
            ));
        }
    }
    Ok(())
}
fn guard(d: &Data, a: &Placement, _: Instant) -> Result<(), Rejection> {
    if a.observed_at > d.at_ms || d.at_ms - a.observed_at > 5000 {
        return Err(Rejection::new(
            "stale",
            "Placement evidence is more than 5 seconds old",
        ));
    }
    if a.priority_revision != d.priority_revision {
        return Err(Rejection::new(
            "priority_changed",
            "Client priorities changed since evaluation",
        ));
    }
    let j = d
        .jobs
        .iter()
        .find(|j| j.id == a.job && j.phase == JobPhase::Queued)
        .ok_or_else(|| Rejection::new("candidate_changed", "Request is no longer queued"))?;
    if a.choice.job(a.job) != a.job || !d.heads().iter().any(|h| h.id == j.id) {
        return Err(Rejection::new(
            "client_fifo",
            "Only the oldest queued request from each client is eligible",
        ));
    }
    if let Some(overdue) = d.overdue_head() {
        if overdue.id != j.id || a.choice == Choice::Defer {
            return Err(Rejection::new(
                "aging",
                "The oldest feasible overdue client head must be served",
            ));
        }
    }
    if let Some(node) = a.choice.node() {
        if !d.nodes.get(node).is_some_and(|n| n.fits(j)) {
            return Err(Rejection::new(
                "insufficient_capacity",
                "CPU or memory is no longer available",
            ));
        }
    }
    Ok(())
}
fn place(d: &mut Data, a: &Placement, _: Instant) -> Result<(), Rejection> {
    if let Some(n) = a.choice.node() {
        let j = d.jobs.iter_mut().find(|j| j.id == a.job).unwrap();
        j.phase = JobPhase::Running;
        j.node = Some(n);
        j.started_at = Some(d.at_ms);
        j.finish_at = Some(d.at_ms + j.actual_duration_ms);
        d.nodes[n].used_cpu += j.cpu;
        d.nodes[n].used_memory_gib += j.memory_gib;
        d.revision += 1;
    }
    Ok(())
}
fn arrive(d: &mut Data, e: &Event, _: Instant) -> Result<(), Rejection> {
    let Event::Arrive(job) = e else { return Ok(()) };
    let mut j = job.clone();
    if !d
        .nodes
        .iter()
        .any(|n| j.cpu <= n.cpu && j.memory_gib <= n.memory_gib)
    {
        j.phase = JobPhase::Rejected;
        j.reason = Some("Larger than every node".into());
    } else if d
        .jobs
        .iter()
        .filter(|j| j.phase == JobPhase::Queued)
        .count()
        >= QUEUE_LIMIT
    {
        j.phase = JobPhase::Rejected;
        j.reason = Some("Queue limit reached (128 jobs)".into());
    }
    d.jobs.push(j);
    d.revision += 1;
    Ok(())
}
fn clock(d: &mut Data, e: &Event, _: Instant) -> Result<(), Rejection> {
    let Event::Clock(at) = e else { return Ok(()) };
    if *at < d.at_ms {
        return Err(Rejection::new("clock", "Time cannot go backward"));
    }
    d.at_ms = *at;
    for j in &mut d.jobs {
        if j.phase == JobPhase::Running && j.finish_at.is_some_and(|t| t <= *at) {
            let n = j.node.unwrap();
            d.nodes[n].used_cpu -= j.cpu;
            d.nodes[n].used_memory_gib -= j.memory_gib;
            j.phase = JobPhase::Completed;
            j.completed_at = j.finish_at.take();
            d.revision += 1;
        }
    }
    Ok(())
}
fn reprioritize(d: &mut Data, e: &Event, _: Instant) -> Result<(), Rejection> {
    if let Event::Priority { client, priority } = e {
        d.priorities.insert(*client, *priority);
        d.priority_revision += 1;
        d.revision += 1;
    }
    Ok(())
}
pub struct Engine {
    machine: StateMachineExecutor<Phase, Data, Placement, Event>,
}
impl Engine {
    pub fn new() -> Result<Self, Error> {
        Self::with_priorities(Default::default())
    }
    pub fn with_priorities(
        priorities: std::collections::BTreeMap<u64, Priority>,
    ) -> Result<Self, Error> {
        let definition = state_machine! { phase:Phase,data:Data,action:Placement,event:Event,invariants:[invariant],transitions:[
            Phase::Scheduling + action(_) => Phase::Scheduling {guard:guard,update:place},
            Phase::Scheduling + event(Event::Arrive(_)) => Phase::Scheduling {update:arrive},
            Phase::Scheduling + event(Event::Clock(_)) => Phase::Scheduling {update:clock},
            Phase::Scheduling + event(Event::Priority { .. }) => Phase::Scheduling {update:reprioritize},
            Phase::Scheduling + evaluation_error(_) => unchanged {},
        ]};
        let nodes = [
            ("Node A", 4, 8),
            ("Node B", 8, 16),
            ("Node C", 8, 16),
            ("Node D", 16, 32),
        ]
        .into_iter()
        .map(|(name, cpu, memory_gib)| Node {
            name: name.into(),
            cpu,
            memory_gib,
            used_cpu: 0,
            used_memory_gib: 0,
        })
        .collect();
        let machine = StateMachineExecutor::builder(definition)
            .name("resource_scheduler")
            .store(InMemory::new(
                Phase::Scheduling,
                Data {
                    priorities,
                    priority_revision: 0,
                    at_ms: 0,
                    revision: 0,
                    jobs: vec![],
                    nodes,
                },
            ))
            .build()
            .map_err(|e| Error::Policy(e.to_string()))?;
        Ok(Self { machine })
    }
    pub fn data(&self) -> Data {
        self.machine.state().expect("in-memory state").1
    }
    async fn event(&mut self, e: Event) -> Result<(), Error> {
        match self
            .machine
            .handle_event(e)
            .await
            .map_err(|e| Error::Policy(e.to_string()))?
        {
            ExecutionOutcome::Applied(_) => Ok(()),
            ExecutionOutcome::Rejected { reason, .. } => Err(Error::Policy(reason.message)),
        }
    }
    pub async fn advance(&mut self, at: u64) -> Result<(), Error> {
        self.event(Event::Clock(at)).await
    }
    pub async fn set_priority(&mut self, client: u64, priority: Priority) -> Result<(), Error> {
        self.event(Event::Priority { client, priority }).await
    }
    pub async fn arrival(&mut self, j: Job) -> Result<(), Error> {
        self.event(Event::Arrive(j)).await
    }
    pub async fn evaluation_error(&mut self, message: &str) -> Result<(), Error> {
        self.machine
            .execute(Err(reflex::EvaluationError::Judge(
                reflex::JudgeError::new("scheduler_inference", message),
            )))
            .await
            .map_err(|e| Error::Policy(e.to_string()))?;
        Ok(())
    }
    pub async fn apply(
        &mut self,
        a: Placement,
        confidence: Option<f64>,
    ) -> Result<(bool, String), Error> {
        let result = self
            .machine
            .execute(
                Judgment {
                    action: a,
                    confidence,
                }
                .try_into(),
            )
            .await
            .map_err(|e| Error::Policy(e.to_string()))?;
        Ok(match result {
            ExecutionOutcome::Applied(r) if r.evaluation_error.is_some() => {
                (false, "Invalid judgment; state retained".into())
            }
            ExecutionOutcome::Applied(_) => (
                true,
                "Freshness, client FIFO, priority revision, aging, CPU and memory guards passed"
                    .into(),
            ),
            ExecutionOutcome::Rejected { reason, .. } => {
                (false, format!("{}: {}", reason.code, reason.message))
            }
        })
    }
}
