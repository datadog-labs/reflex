// Unless explicitly stated otherwise all files in this repository are licensed under the Apache-2.0 License.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-present Datadog, Inc.

use crate::{
    policy::{
        Admission, AdmissionContext, CircuitPhase, ClientOutcome, Observation, Policy,
        PolicyFactory,
    },
    scenario::{Environment, Scenario},
    trace::Trace,
    Error,
};
use serde::Serialize;
use std::{
    cmp::Ordering,
    collections::{BinaryHeap, VecDeque},
};

#[derive(Debug, Clone, Default, Serialize)]
pub struct Counts {
    pub offered: u64,
    pub admitted: u64,
    pub success: u64,
    pub error: u64,
    pub timeout: u64,
    pub shed: u64,
}
#[derive(Debug, Clone, Serialize)]
pub struct Record {
    pub id: usize,
    pub service: usize,
    pub arrived_ms: f64,
    pub started_ms: Option<f64>,
    pub client_finished_ms: Option<f64>,
    pub downstream_finished_ms: Option<f64>,
    pub outcome: Option<ClientOutcome>,
    pub downstream_success: Option<bool>,
    /// Simulated server response: 200 success, 500 processing failure, 503 queue full.
    pub http_status_code: Option<u16>,
    pub probe: bool,
    pub work_ms: f64,
    pub post_timeout_work_ms: f64,
    pub peak_stress: f64,
    pub cause: Option<String>,
}
#[derive(Debug, Clone, Serialize)]
pub struct Snapshot {
    pub at_ms: f64,
    pub service: usize,
    pub phase: CircuitPhase,
    pub active: usize,
    pub queued: usize,
    pub stress: f64,
    pub latency_multiplier: f64,
    pub injected_error_probability: f64,
    pub offered_rate: f64,
    pub counts: Counts,
}
#[derive(Debug, Clone, Serialize)]
pub struct Transition {
    pub at_ms: f64,
    pub service: usize,
    pub from: CircuitPhase,
    pub to: CircuitPhase,
    pub trigger: String,
}
#[derive(Debug, Clone, Serialize)]
pub struct Metrics {
    pub counts: Counts,
    pub success_p95_ms: Option<f64>,
    pub client_p95_ms: Option<f64>,
    pub work_ms: f64,
    pub wasted_work_ms: f64,
    pub post_timeout_work_ms: f64,
    pub peak_queue: usize,
    pub peak_stress: f64,
    pub open_ms: f64,
    pub half_open_ms: f64,
}
#[derive(Debug, Clone, Serialize)]
pub struct Run {
    pub algorithm: String,
    pub name: String,
    pub description: String,
    pub scenario: String,
    pub seed: u64,
    pub trace_fingerprint: String,
    pub duration_ms: f64,
    pub drained_at_ms: f64,
    pub metrics: Metrics,
    pub service_metrics: Vec<Metrics>,
    pub snapshots: Vec<Snapshot>,
    pub transitions: Vec<Transition>,
    pub requests: Vec<Record>,
}
struct Job {
    remaining: f64,
    generation: u64,
    peak_error: f64,
}
struct Server {
    policy: Box<dyn Policy>,
    env: Environment,
    active: Vec<usize>,
    queue: VecDeque<usize>,
    stress: f64,
    counts: Counts,
    generation: u64,
    peak_queue: usize,
    peak_stress: f64,
    open_ms: f64,
    half_open_ms: f64,
}
#[derive(Debug, Clone, Copy)]
enum Kind {
    Boundary,
    Complete { service: usize, generation: u64 },
    Deadline(usize),
    Arrival(usize),
    Sample,
}
impl Kind {
    fn rank(self) -> u8 {
        match self {
            Self::Boundary => 0,
            Self::Complete { .. } => 1,
            Self::Deadline(_) => 2,
            Self::Arrival(_) => 3,
            Self::Sample => 4,
        }
    }
}
#[derive(Debug, Clone, Copy)]
struct Event {
    at: f64,
    sequence: u64,
    kind: Kind,
}
impl PartialEq for Event {
    fn eq(&self, o: &Self) -> bool {
        self.at == o.at && self.kind.rank() == o.kind.rank() && self.sequence == o.sequence
    }
}
impl Eq for Event {}
impl PartialOrd for Event {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}
impl Ord for Event {
    fn cmp(&self, o: &Self) -> Ordering {
        o.at.total_cmp(&self.at)
            .then(o.kind.rank().cmp(&self.kind.rank()))
            .then(o.sequence.cmp(&self.sequence))
    }
}
struct World {
    scenario: Scenario,
    trace: Trace,
    now: f64,
    events: BinaryHeap<Event>,
    sequence: u64,
    servers: Vec<Server>,
    jobs: Vec<Option<Job>>,
    records: Vec<Record>,
    snapshots: Vec<Snapshot>,
    transitions: Vec<Transition>,
    drained_at: f64,
    telemetry: Option<crate::telemetry::Telemetry>,
}
impl World {
    fn push(&mut self, at: f64, kind: Kind) {
        self.events.push(Event {
            at,
            kind,
            sequence: self.sequence,
        });
        self.sequence += 1;
    }
    fn schedule(&mut self, sid: usize) {
        let server = &mut self.servers[sid];
        server.generation += 1;
        let remaining = server
            .active
            .iter()
            .map(|id| self.jobs[*id].as_ref().unwrap().remaining)
            .fold(f64::INFINITY, f64::min);
        if remaining.is_finite() {
            let at = self.now + remaining.max(0.0) * server.env.latency_multiplier;
            let generation = server.generation;
            self.push(
                at,
                Kind::Complete {
                    service: sid,
                    generation,
                },
            );
        }
    }
    fn advance(&mut self, at: f64) {
        let dt = (at - self.now).max(0.0);
        for (sid, s) in self.servers.iter_mut().enumerate() {
            let nominal = self.scenario.services[sid].workers as f64;
            let target =
                (((s.active.len() + s.queue.len()) as f64 / nominal - 0.75) / 2.0).clamp(0.0, 1.0);
            let tau = if target > s.stress {
                self.scenario.stress_build_ms
            } else {
                self.scenario.stress_recovery_ms
            };
            let next = target + (s.stress - target) * (-dt / tau).exp();
            let peak = s.stress.max(next);
            let observed_dt = (at.min(self.scenario.duration_ms)
                - self.now.min(self.scenario.duration_ms))
            .max(0.0);
            match s.policy.phase() {
                CircuitPhase::Open => s.open_ms += observed_dt,
                CircuitPhase::HalfOpen => s.half_open_ms += observed_dt,
                _ => {}
            }
            for id in &s.active {
                let j = self.jobs[*id].as_mut().unwrap();
                j.remaining = (j.remaining - dt / s.env.latency_multiplier).max(0.0);
                j.peak_error = j.peak_error.max(s.env.error_probability);
                let record = &mut self.records[*id];
                record.work_ms += dt;
                record.peak_stress = record.peak_stress.max(peak);
                if record.outcome == Some(ClientOutcome::Timeout) {
                    record.post_timeout_work_ms += dt;
                }
            }
            s.stress = next;
            s.peak_stress = s.peak_stress.max(peak);
        }
        self.now = at;
    }
    fn transition(&mut self, sid: usize, before: CircuitPhase, trigger: &str) {
        let after = self.servers[sid].policy.phase();
        if before != after {
            if let Some(telemetry) = &self.telemetry {
                telemetry.transitioned(sid, before, after);
                tracing::info!(target: "reflex_sim::circuit_breaker", upstream = self.scenario.services[sid].id,
                    from = crate::telemetry::phase(before), to = crate::telemetry::phase(after),
                    trigger, simulation_time_ms = self.now, "Circuit breaker changed state");
            }
            self.transitions.push(Transition {
                at_ms: self.now,
                service: sid,
                from: before,
                to: after,
                trigger: trigger.into(),
            });
        }
    }
    async fn finish_client(
        &mut self,
        id: usize,
        outcome: ClientOutcome,
        cause: &str,
    ) -> Result<(), Error> {
        if self.records[id].outcome.is_some() {
            return Ok(());
        }
        let sid = self.trace.requests[id].service;
        let record = &mut self.records[id];
        record.outcome = Some(outcome);
        record.client_finished_ms = Some(self.now);
        record.cause = Some(cause.into());
        if let Some(telemetry) = &self.telemetry {
            telemetry.client_finished(
                sid,
                outcome,
                record.http_status_code,
                self.now - record.arrived_ms,
                record.probe,
            );
        }
        let s = &mut self.servers[sid];
        match outcome {
            ClientOutcome::Success => s.counts.success += 1,
            ClientOutcome::Error => s.counts.error += 1,
            ClientOutcome::Timeout => s.counts.timeout += 1,
            ClientOutcome::Shed => s.counts.shed += 1,
        }
        if outcome != ClientOutcome::Shed {
            let observation = Observation {
                at_ms: self.now,
                request_id: id,
                outcome,
                latency_ms: self.now - record.arrived_ms,
                generation: self.jobs[id].as_ref().unwrap().generation,
            };
            let before = s.policy.phase();
            s.policy.observe(observation).await?;
            self.transition(
                sid,
                before,
                match outcome {
                    ClientOutcome::Success => "Successful response",
                    ClientOutcome::Timeout => "Client deadline",
                    _ => "Error response",
                },
            );
        }
        Ok(())
    }
    fn start(&mut self, sid: usize, id: usize) {
        if let Some(telemetry) = &self.telemetry {
            telemetry.started(sid, self.now - self.records[id].arrived_ms);
        }
        self.records[id].started_ms = Some(self.now);
        self.records[id].peak_stress = self.records[id].peak_stress.max(self.servers[sid].stress);
        self.servers[sid].active.push(id);
    }
    async fn arrival(&mut self, id: usize) -> Result<(), Error> {
        let sid = self.trace.requests[id].service;
        let s = &mut self.servers[sid];
        s.counts.offered += 1;
        let before = s.policy.phase();
        let admission = s
            .policy
            .admit(AdmissionContext {
                at_ms: self.now,
                request_id: id,
            })
            .await?;
        self.transition(sid, before, "Admission decision");
        let Admission::Allow { generation, probe } = admission else {
            return self
                .finish_client(id, ClientOutcome::Shed, "Circuit blocked admission")
                .await;
        };
        self.servers[sid].counts.admitted += 1;
        self.records[id].probe = probe;
        self.jobs[id] = Some(Job {
            remaining: self.trace.requests[id].work_ms,
            generation,
            peak_error: 0.0,
        });
        if self.servers[sid].active.len() < self.scenario.services[sid].workers {
            self.start(sid, id);
            self.schedule(sid);
        } else if self.servers[sid].queue.len() < self.scenario.services[sid].queue_limit {
            self.servers[sid].queue.push_back(id);
            self.servers[sid].peak_queue = self.servers[sid]
                .peak_queue
                .max(self.servers[sid].queue.len());
        } else {
            self.records[id].http_status_code = Some(503);
            if let Some(telemetry) = &self.telemetry {
                telemetry.server_finished(sid, 503, self.now - self.records[id].arrived_ms);
            }
            self.finish_client(id, ClientOutcome::Error, "Downstream queue full")
                .await?;
            self.records[id].downstream_finished_ms = Some(self.now);
            self.records[id].downstream_success = Some(false);
            self.jobs[id] = None;
            return Ok(());
        }
        self.push(self.now + self.scenario.timeout_ms, Kind::Deadline(id));
        Ok(())
    }
    async fn complete(&mut self, sid: usize) -> Result<(), Error> {
        let index = self.servers[sid]
            .active
            .iter()
            .enumerate()
            .min_by(|(_, a), (_, b)| {
                self.jobs[**a]
                    .as_ref()
                    .unwrap()
                    .remaining
                    .total_cmp(&self.jobs[**b].as_ref().unwrap().remaining)
            })
            .map(|(i, _)| i)
            .unwrap();
        let id = self.servers[sid].active.remove(index);
        let job = self.jobs[id].as_ref().unwrap();
        let injected = job.peak_error.max(self.servers[sid].env.error_probability);
        let stress = self.records[id].peak_stress.max(self.servers[sid].stress);
        let probability = 1.0
            - (1.0 - injected) * (1.0 - self.scenario.stress_error_probability * stress * stress);
        let success = self.trace.requests[id].error_roll >= probability;
        self.records[id].peak_stress = stress;
        self.records[id].downstream_finished_ms = Some(self.now);
        self.records[id].downstream_success = Some(success);
        let status = if success { 200 } else { 500 };
        self.records[id].http_status_code = Some(status);
        if let Some(telemetry) = &self.telemetry {
            telemetry.server_finished(sid, status, self.now - self.records[id].arrived_ms);
        }
        self.drained_at = self.drained_at.max(self.now);
        self.finish_client(
            id,
            if success {
                ClientOutcome::Success
            } else {
                ClientOutcome::Error
            },
            if success {
                "Response before deadline"
            } else {
                "Injected error or accumulated load stress"
            },
        )
        .await?;
        self.jobs[id] = None;
        if let Some(next) = self.servers[sid].queue.pop_front() {
            self.start(sid, next);
        }
        self.schedule(sid);
        Ok(())
    }
    fn refresh_telemetry(&self) {
        if let Some(telemetry) = &self.telemetry {
            telemetry.snapshot(self.servers.iter().enumerate().map(|(sid, s)| {
                let finished = s.counts.success + s.counts.error + s.counts.timeout + s.counts.shed;
                crate::telemetry::Gauges {
                    client_in_flight: s.counts.offered - finished,
                    queued: s.queue.len() as u64,
                    active: s.active.len() as u64,
                    utilization: s.active.len() as f64 / self.scenario.services[sid].workers as f64,
                    timed_out_work: s
                        .active
                        .iter()
                        .filter(|id| self.records[**id].outcome == Some(ClientOutcome::Timeout))
                        .count() as u64,
                    phase: s.policy.phase(),
                }
            }));
        }
    }
    fn sample(&mut self) {
        for (sid, s) in self.servers.iter().enumerate() {
            self.snapshots.push(Snapshot {
                at_ms: self.now,
                service: sid,
                phase: s.policy.phase(),
                active: s.active.len(),
                queued: s.queue.len(),
                stress: s.stress,
                latency_multiplier: s.env.latency_multiplier,
                injected_error_probability: s.env.error_probability,
                offered_rate: s.env.rate,
                counts: s.counts.clone(),
            });
        }
    }
}
fn percentile(mut values: Vec<f64>) -> Option<f64> {
    if values.is_empty() {
        None
    } else {
        values.sort_by(f64::total_cmp);
        Some(values[(values.len() as f64 * 0.95).ceil() as usize - 1])
    }
}
fn metrics(records: &[Record], servers: &[Server], service: Option<usize>) -> Metrics {
    let selected: Vec<_> = records
        .iter()
        .filter(|r| service.is_none_or(|s| r.service == s))
        .collect();
    let mut counts = Counts::default();
    for r in &selected {
        counts.offered += 1;
        match r.outcome.unwrap() {
            ClientOutcome::Success => counts.success += 1,
            ClientOutcome::Error => counts.error += 1,
            ClientOutcome::Timeout => counts.timeout += 1,
            ClientOutcome::Shed => counts.shed += 1,
        };
        if r.outcome != Some(ClientOutcome::Shed) {
            counts.admitted += 1;
        }
    }
    let ss: Vec<_> = servers
        .iter()
        .enumerate()
        .filter(|(i, _)| service.is_none_or(|s| s == *i))
        .map(|(_, s)| s)
        .collect();
    Metrics {
        counts,
        success_p95_ms: percentile(
            selected
                .iter()
                .filter(|r| r.outcome == Some(ClientOutcome::Success))
                .map(|r| r.client_finished_ms.unwrap() - r.arrived_ms)
                .collect(),
        ),
        client_p95_ms: percentile(
            selected
                .iter()
                .filter(|r| r.outcome != Some(ClientOutcome::Shed))
                .map(|r| r.client_finished_ms.unwrap() - r.arrived_ms)
                .collect(),
        ),
        work_ms: selected.iter().map(|r| r.work_ms).sum(),
        wasted_work_ms: selected
            .iter()
            .filter(|r| r.outcome != Some(ClientOutcome::Success))
            .map(|r| r.work_ms)
            .sum(),
        post_timeout_work_ms: selected.iter().map(|r| r.post_timeout_work_ms).sum(),
        peak_queue: ss.iter().map(|s| s.peak_queue).max().unwrap_or(0),
        peak_stress: ss.iter().map(|s| s.peak_stress).fold(0.0, f64::max),
        open_ms: ss.iter().map(|s| s.open_ms).sum(),
        half_open_ms: ss.iter().map(|s| s.half_open_ms).sum(),
    }
}
/// Replay a generated trace. Fault boundaries affect work already in flight.
/// Ties: environment change, completion, deadline, arrival, then sampling.
pub async fn simulate(
    scenario: &Scenario,
    trace: &Trace,
    factory: PolicyFactory,
) -> Result<Run, Error> {
    simulate_inner(scenario, trace, factory, None).await
}

/// Replay with an application-owned telemetry meter. Durations use simulated time.
pub async fn simulate_with_meter(
    scenario: &Scenario,
    trace: &Trace,
    factory: PolicyFactory,
    meter: opentelemetry::metrics::Meter,
) -> Result<Run, Error> {
    simulate_inner(scenario, trace, factory, Some(meter)).await
}
async fn simulate_inner(
    scenario: &Scenario,
    trace: &Trace,
    factory: PolicyFactory,
    meter: Option<opentelemetry::metrics::Meter>,
) -> Result<Run, Error> {
    let mut w = build_world(scenario, trace, factory)?;
    if let Some(meter) = meter {
        w.telemetry = Some(crate::telemetry::Telemetry::new(
            meter,
            &scenario.services,
            factory.id,
        ));
        w.refresh_telemetry();
    }
    for r in &trace.requests {
        w.push(r.at_ms, Kind::Arrival(r.id));
    }
    for phase in &scenario.phases {
        w.push(phase.end_ms, Kind::Boundary);
    }
    w.push(0.0, Kind::Sample);
    while let Some(e) = w.events.pop() {
        w.step(e).await?;
    }
    if w.records.iter().any(|r| r.outcome.is_none()) || w.jobs.iter().any(Option::is_some) {
        return Err(Error::Invalid(
            "simulation ended with unfinished requests".into(),
        ));
    }
    let all = metrics(&w.records, &w.servers, None);
    let service_metrics = (0..w.servers.len())
        .map(|i| metrics(&w.records, &w.servers, Some(i)))
        .collect();
    Ok(Run {
        algorithm: factory.id.into(),
        name: factory.name.into(),
        description: factory.description.into(),
        scenario: scenario.id.clone(),
        seed: trace.seed,
        trace_fingerprint: trace.fingerprint.clone(),
        duration_ms: scenario.duration_ms,
        drained_at_ms: w.drained_at,
        metrics: all,
        service_metrics,
        snapshots: w.snapshots,
        transitions: w.transitions,
        requests: w.records,
    })
}

fn build_world(scenario: &Scenario, trace: &Trace, factory: PolicyFactory) -> Result<World, Error> {
    scenario.validate()?;
    if trace.scenario != scenario.id
        || trace.requests.len() > 200_000
        || trace.requests.iter().enumerate().any(|(i, r)| {
            r.id != i
                || r.service >= scenario.services.len()
                || !r.at_ms.is_finite()
                || r.at_ms < 0.0
                || r.at_ms >= scenario.duration_ms
                || !r.work_ms.is_finite()
                || r.work_ms <= 0.0
                || r.work_ms > 60_000.0
                || !(0.0..1.0).contains(&r.error_roll)
        })
    {
        return Err(Error::Invalid(
            "trace does not match scenario or contains invalid requests".into(),
        ));
    }
    let servers = scenario
        .services
        .iter()
        .enumerate()
        .map(|(i, _)| {
            Ok(Server {
                policy: (factory.create)()?,
                env: scenario.environment(i, 0.0),
                active: vec![],
                queue: VecDeque::new(),
                stress: 0.0,
                counts: Counts::default(),
                generation: 0,
                peak_queue: 0,
                peak_stress: 0.0,
                open_ms: 0.0,
                half_open_ms: 0.0,
            })
        })
        .collect::<Result<Vec<_>, Error>>()?;
    let records = trace
        .requests
        .iter()
        .map(|r| Record {
            id: r.id,
            service: r.service,
            arrived_ms: r.at_ms,
            started_ms: None,
            client_finished_ms: None,
            downstream_finished_ms: None,
            outcome: None,
            downstream_success: None,
            http_status_code: None,
            probe: false,
            work_ms: 0.0,
            post_timeout_work_ms: 0.0,
            peak_stress: 0.0,
            cause: None,
        })
        .collect();
    Ok(World {
        scenario: scenario.clone(),
        trace: trace.clone(),
        now: 0.0,
        events: BinaryHeap::new(),
        sequence: 0,
        servers,
        jobs: (0..trace.requests.len()).map(|_| None).collect(),
        records,
        snapshots: vec![],
        transitions: vec![],
        drained_at: scenario.duration_ms,
        telemetry: None,
    })
}

impl World {
    async fn step(&mut self, e: Event) -> Result<(), Error> {
        if let Kind::Complete {
            service,
            generation,
        } = e.kind
        {
            if self.servers[service].generation != generation {
                return Ok(());
            }
        }
        if let Kind::Deadline(id) = e.kind {
            if self.records[id].outcome.is_some() {
                return Ok(());
            }
        }
        self.advance(e.at);
        match e.kind {
            Kind::Boundary => {
                for i in 0..self.servers.len() {
                    self.servers[i].env = self.scenario.environment(i, self.now);
                    self.schedule(i);
                }
            }
            Kind::Arrival(id) => self.arrival(id).await?,
            Kind::Complete { service, .. } => self.complete(service).await?,
            Kind::Deadline(id) => {
                self.finish_client(
                    id,
                    ClientOutcome::Timeout,
                    "Client deadline; downstream work continues",
                )
                .await?
            }
            Kind::Sample => {
                self.sample();
                if self.now < self.scenario.duration_ms {
                    self.push(
                        (self.now + self.scenario.sample_ms).min(self.scenario.duration_ms),
                        Kind::Sample,
                    );
                }
            }
        }
        self.refresh_telemetry();
        Ok(())
    }
    async fn run_until(&mut self, target: f64) -> Result<(), Error> {
        while self.events.peek().is_some_and(|e| e.at <= target) {
            let e = self.events.pop().unwrap();
            self.step(e).await?;
        }
        self.advance(target);
        self.refresh_telemetry();
        Ok(())
    }
}

pub mod live;
