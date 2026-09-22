//! Incremental use of the same queue, deadline, stress, and Reflex policy engine.
use super::*;
use crate::jev::{Evidence, GuardResult, Inference, JevPolicy, PolicyKind};
use crate::{
    generate_trace,
    policy::builtins,
    presets,
    scenario::Service,
    trace::{Random, Request},
};
use serde::Deserialize;

pub const HORIZON_MS: f64 = 180_000.0;
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Faults {
    pub slow: bool,
    pub errors: bool,
    pub surge: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Injection {
    pub at_ms: f64,
    pub service: usize,
    pub faults: Faults,
}
#[derive(Serialize)]
pub struct ServiceView {
    pub definition: Service,
    pub state: Snapshot,
    pub faults: Faults,
    pub timed_out_work: usize,
}
#[derive(Serialize)]
pub struct LiveView {
    pub seed: u64,
    pub at_ms: f64,
    pub horizon_ms: f64,
    pub services: Vec<ServiceView>,
    pub counts: Counts,
    pub transitions: Vec<Transition>,
    pub injections: Vec<Injection>,
    pub requests: Vec<Record>,
    pub history: Vec<Snapshot>,
    pub post_timeout_work_ms: f64,
}
#[derive(Serialize)]
pub struct IncidentExport<'a> {
    pub model: &'static str,
    pub algorithm: &'static str,
    pub seed: u64,
    pub at_ms: f64,
    pub scenario: &'a Scenario,
    pub injections: &'a [Injection],
    pub trace: &'a Trace,
    pub requests: &'a [Record],
    pub transitions: &'a [Transition],
    pub snapshots: &'a [Snapshot],
}

pub struct LiveSimulation {
    world: World,
    policy: PolicyKind,
    candidates: Vec<Request>,
    acceptance: Vec<f64>,
    next_candidate: usize,
    faults: Vec<Faults>,
    injections: Vec<Injection>,
}
impl LiveSimulation {
    pub fn new(seed: u64) -> Result<Self, Error> {
        Self::with_policy(seed, PolicyKind::Threshold)
    }
    pub fn with_policy(seed: u64, policy: PolicyKind) -> Result<Self, Error> {
        Self::with_policy_and_meter(
            seed,
            policy,
            opentelemetry::global::meter("circuit_breaker"),
        )
    }
    /// Supply an application-owned meter; exporters remain outside the simulation.
    pub fn with_policy_and_meter(
        seed: u64,
        policy: PolicyKind,
        meter: opentelemetry::metrics::Meter,
    ) -> Result<Self, Error> {
        Self::configured_meter(seed, policy, meter, None)
    }
    pub fn with_datadog(seed: u64, run: &str) -> Result<Self, Error> {
        Self::configured_meter(
            seed,
            PolicyKind::Jev,
            opentelemetry::global::meter("circuit_breaker"),
            Some(run),
        )
    }
    fn configured_meter(
        seed: u64,
        policy: PolicyKind,
        meter: opentelemetry::metrics::Meter,
        run: Option<&str>,
    ) -> Result<Self, Error> {
        let mut scenario = presets().remove(0);
        scenario.id = "incident-playground".into();
        scenario.name = "Incident Playground".into();
        scenario.duration_ms = if run.is_some() { 600_000. } else { HORIZON_MS };
        scenario.phases.truncate(1);
        scenario.phases[0].end_ms = scenario.duration_ms;
        let trace = Trace {
            model_version: "queue-stress-live-v1".into(),
            scenario: scenario.id.clone(),
            seed,
            fingerprint: String::new(),
            requests: vec![],
        };
        let factory = match policy {
            PolicyKind::Threshold => builtins()[1],
            PolicyKind::Jev => PolicyFactory {
                id: "jev",
                name: "Reflex · Jev",
                description: "Jev recommendations with deterministic Reflex guards",
                create: if run.is_some() {
                    || Ok(Box::new(JevPolicy::datadog()?))
                } else {
                    || Ok(Box::new(JevPolicy::new()?))
                },
            },
        };
        let mut world = build_world(&scenario, &trace, factory)?;
        world.telemetry = Some(crate::telemetry::Telemetry::with_run(
            meter,
            &scenario.services,
            factory.id,
            run,
        ));
        world.refresh_telemetry();
        world.sample();
        world.push(scenario.sample_ms, Kind::Sample);
        // A maximum-rate Poisson stream is thinned at the current offered rate.
        // Changing traffic never reschedules or redraws previous requests.
        for service in &mut scenario.services {
            service.rate *= 4.0;
        }
        let candidates = generate_trace(&scenario, seed)?.requests;
        let mut rng = Random(seed ^ 0xa82b_f379_1329_fdeb);
        let acceptance = candidates.iter().map(|_| rng.next()).collect();
        Ok(Self {
            world,
            policy,
            candidates,
            acceptance,
            next_candidate: 0,
            faults: vec![Faults::default(); scenario.services.len()],
            injections: vec![],
        })
    }
    pub(crate) fn without_telemetry(mut self) -> Self {
        self.world.telemetry = None;
        self
    }
    pub(crate) fn service_id(&self, service: usize) -> &str {
        &self.world.scenario.services[service].id
    }
    pub fn policy(&self) -> PolicyKind {
        self.policy
    }
    pub fn evidence(&self) -> Vec<(usize, Evidence)> {
        self.world
            .servers
            .iter()
            .enumerate()
            .filter_map(|(i, s)| {
                s.policy.evidence(self.now()).map(|mut e| {
                    e.service = self.world.scenario.services[i].name.clone();
                    e.client_timeout_ms = self.world.scenario.timeout_ms;
                    (i, e)
                })
            })
            .collect()
    }
    pub async fn apply_model(
        &mut self,
        service: usize,
        evidence: Evidence,
        inference: Inference,
    ) -> Result<GuardResult, Error> {
        let now = self.now();
        let server = self
            .world
            .servers
            .get_mut(service)
            .ok_or_else(|| Error::Invalid("unknown downstream".into()))?;
        let before = server.policy.phase();
        let result = server.policy.apply_model(now, evidence, inference).await?;
        self.world
            .transition(service, before, "Jev recommendation accepted by Reflex");
        self.world.refresh_telemetry();
        Ok(result)
    }
    pub fn horizon(&self) -> f64 {
        self.world.scenario.duration_ms
    }
    pub fn now(&self) -> f64 {
        self.world.now
    }
    pub fn seed(&self) -> u64 {
        self.world.trace.seed
    }
    pub fn injections(&self) -> &[Injection] {
        &self.injections
    }
    pub fn set_faults(&mut self, service: usize, faults: Faults) -> Result<(), Error> {
        if service >= self.faults.len() {
            return Err(Error::Invalid("unknown downstream".into()));
        }
        if self.faults[service] == faults {
            return Ok(());
        }
        if self.injections.len() >= 500 {
            return Err(Error::Invalid(
                "incident contains 500 edits; reset to start a new incident".into(),
            ));
        }
        self.faults[service] = faults;
        self.injections.push(Injection {
            at_ms: self.now(),
            service,
            faults,
        });
        let base = &self.world.scenario.services[service];
        self.world.servers[service].env = Environment {
            rate: base.rate * if faults.surge { 4.0 } else { 1.0 },
            latency_multiplier: if faults.slow { 6.0 } else { 1.0 },
            error_probability: 1.0
                - (1.0 - base.base_error_probability) * if faults.errors { 0.15 } else { 1.0 },
        };
        if self.world.telemetry.is_some() {
            tracing::info!(target: "reflex_sim::incident", upstream = base.id,
                slow = faults.slow, errors = faults.errors, surge = faults.surge,
                simulation_time_ms = self.now(), "Service faults updated");
        }
        // Recompute completion time for existing work. Neither queues nor stress reset.
        self.world.schedule(service);
        Ok(())
    }
    pub fn repair_all(&mut self) -> Result<(), Error> {
        let edits = self
            .faults
            .iter()
            .filter(|f| **f != Faults::default())
            .count();
        if self.injections.len() + edits > 500 {
            return Err(Error::Invalid(
                "incident edit limit reached; reset to start a new incident".into(),
            ));
        }
        for service in 0..self.faults.len() {
            self.set_faults(service, Faults::default())?;
        }
        Ok(())
    }
    pub async fn advance_to(&mut self, target: f64) -> Result<(), Error> {
        if !target.is_finite() || target < self.now() || target > self.horizon() {
            return Err(Error::Invalid(
                "playground time must advance within the configured horizon".into(),
            ));
        }
        if target == self.now() {
            return Ok(());
        }
        while self
            .candidates
            .get(self.next_candidate)
            .is_some_and(|r| r.at_ms <= target)
        {
            let index = self.next_candidate;
            let mut request = self.candidates[index].clone();
            self.next_candidate += 1;
            self.world.run_until(request.at_ms).await?;
            if !self.faults[request.service].surge && self.acceptance[index] >= 0.25 {
                continue;
            }
            request.id = self.world.records.len();
            let id = request.id;
            self.world.records.push(Record {
                id,
                service: request.service,
                arrived_ms: request.at_ms,
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
            });
            self.world.trace.requests.push(request);
            self.world.jobs.push(None);
            self.world.arrival(id).await?;
        }
        self.world.run_until(target).await
    }
    pub fn view(&self) -> LiveView {
        let w = &self.world;
        let services = w
            .servers
            .iter()
            .enumerate()
            .map(|(service, s)| ServiceView {
                definition: w.scenario.services[service].clone(),
                faults: self.faults[service],
                timed_out_work: s
                    .active
                    .iter()
                    .chain(s.queue.iter())
                    .filter(|id| w.records[**id].outcome == Some(ClientOutcome::Timeout))
                    .count(),
                state: Snapshot {
                    at_ms: w.now,
                    service,
                    phase: s.policy.phase(),
                    active: s.active.len(),
                    queued: s.queue.len(),
                    stress: s.stress,
                    latency_multiplier: s.env.latency_multiplier,
                    injected_error_probability: s.env.error_probability,
                    offered_rate: s.env.rate,
                    counts: s.counts.clone(),
                },
            })
            .collect();
        let mut counts = Counts::default();
        for s in &w.servers {
            counts.offered += s.counts.offered;
            counts.admitted += s.counts.admitted;
            counts.success += s.counts.success;
            counts.error += s.counts.error;
            counts.timeout += s.counts.timeout;
            counts.shed += s.counts.shed;
        }
        LiveView {
            seed: w.trace.seed,
            at_ms: w.now,
            horizon_ms: self.horizon(),
            services,
            counts,
            transitions: w.transitions.iter().rev().take(40).cloned().collect(),
            injections: self.injections.iter().rev().take(30).cloned().collect(),
            requests: w.records.iter().rev().take(100).cloned().collect(),
            history: w
                .snapshots
                .iter()
                .filter(|s| s.at_ms >= w.now - 30_000.0)
                .cloned()
                .collect(),
            post_timeout_work_ms: w.records.iter().map(|r| r.post_timeout_work_ms).sum(),
        }
    }
    pub fn export(&self) -> IncidentExport<'_> {
        IncidentExport {
            model: "queue-stress-live-v1",
            algorithm: match self.policy {
                PolicyKind::Threshold => "reflex-threshold-v1",
                PolicyKind::Jev => "reflex-jev-v1",
            },
            seed: self.seed(),
            at_ms: self.now(),
            scenario: &self.world.scenario,
            injections: &self.injections,
            trace: &self.world.trace,
            requests: &self.world.records,
            transitions: &self.world.transitions,
            snapshots: &self.world.snapshots,
        }
    }
}
