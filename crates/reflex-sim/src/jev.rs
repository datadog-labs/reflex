//! Jev recommends; the Reflex executor owns legality, freshness, and probe bounds.
use crate::{
    policy::{
        Admission, AdmissionContext, CircuitPhase, ClientOutcome, Observation, Policy, PolicyFuture,
    },
    Error,
};
use reflex::{
    state_machine, Controller, EvaluationError, ExecutionOutcome, InMemory, Judgment, Rejection,
    StateMachineExecutor,
};
use reflex_typesafe::TypeSafeJudge;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, VecDeque},
    future::Future,
    pin::Pin,
    sync::Arc,
    time::{Duration, Instant},
};
use typesafe_ai::{choice, questions, SystemOneTask, TypeSafeClient, Usage};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum PolicyKind {
    #[default]
    Threshold,
    Jev,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Choice {
    Open,
    Probe,
    NoChange,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Window {
    pub responses: usize,
    pub successes: usize,
    pub errors: usize,
    pub timeouts: usize,
    pub failure_ratio: f64,
    pub p95_latency_ms: Option<f64>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Evidence {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forecast: Option<crate::forecasting::Evidence>,
    pub service: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub telemetry: Option<crate::datadog::TelemetryEvidence>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub control_since_unix_ms: u64,
    pub observed_at_ms: f64,
    pub phase: CircuitPhase,
    pub revision: u64,
    pub last_5_seconds: Window,
    pub last_1_second: Window,
    pub cooldown_remaining_ms: f64,
    pub client_timeout_ms: f64,
    pub legal_actions: Vec<Choice>,
}
fn is_zero(value: &u64) -> bool {
    *value == 0
}
impl Evidence {
    pub fn model_input(&self) -> serde_json::Value {
        if let Some(telemetry) = &self.telemetry {
            serde_json::json!({"service":self.service,"control":{"phase":self.phase,"revision":self.revision,
                "cooldown_remaining_ms":self.cooldown_remaining_ms,"client_timeout_ms":self.client_timeout_ms,
                "legal_actions":self.legal_actions},"telemetry":telemetry,"forecast":self.forecast})
        } else {
            serde_json::to_value(self).expect("finite evidence")
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Failure {
    pub code: String,
    pub message: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Inference {
    pub action: Option<Choice>,
    pub confidence: Option<f64>,
    pub probabilities: BTreeMap<String, f64>,
    pub model: Option<String>,
    pub usage: Option<Usage>,
    pub request_id: Option<String>,
    pub error: Option<Failure>,
    pub wall_latency_ms: f64,
}
impl Inference {
    pub fn failed(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            action: None,
            confidence: None,
            probabilities: BTreeMap::new(),
            model: None,
            usage: None,
            request_id: None,
            error: Some(Failure {
                code: code.into(),
                message: message.into(),
            }),
            wall_latency_ms: 0.0,
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GuardResult {
    pub status: String,
    pub reason: String,
    pub from: CircuitPhase,
    pub to: CircuitPhase,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecisionRecord {
    pub id: u64,
    pub service: usize,
    pub completed_at_ms: f64,
    pub evidence: Evidence,
    #[serde(default)]
    pub model_input: serde_json::Value,
    pub inference: Inference,
    pub guard: GuardResult,
}
pub type EvaluationFuture<'a> = Pin<Box<dyn Future<Output = Inference> + Send + 'a>>;
pub trait Evaluator: Send + Sync {
    fn evidence_source(&self) -> Option<Arc<crate::datadog::Source>> {
        None
    }
    fn evaluate(&self, evidence: Evidence) -> EvaluationFuture<'_>;
}
#[derive(Clone)]
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
    fn evaluate(&self, evidence: Evidence) -> EvaluationFuture<'_> {
        Box::pin(async move {
            let start = Instant::now();
            let task=SystemOneTask::builder().model(&self.model).questions(questions! {
                action: choice("Protect useful traffic in this circuit breaker. When forecast is present, use its uncertain p10/p50/p90 projections of recent observations to anticipate pressure. Forecasts cannot predict injected faults or certify recovery, and lower observed failures while blocking are not proof of health. Never bypass a recovery probe based on forecasts. Use only the supplied client-visible evidence. Compare the supplied short and long windows, respecting their explicit durations and timestamps. Datadog measurements are delayed and may be missing; never interpret missing data as health. Locally blocked requests are not downstream failures. Server queue and utilization measurements, when present, describe resource pressure. Opening blocks requests and lets downstream work drain; blocking healthy traffic loses useful work. Once open and cooldown has elapsed, recommend a probe to test recovery. A successful probe automatically closes the circuit; a failed probe reopens it. Select only a legal action. NoChange means maintain the current state or abstain when evidence is weak. Confidence, if supplied, describes the choice, not guaranteed operational success.", [
                    (Choice::Open,"Open the closed circuit to relieve sustained distress"),
                    (Choice::Probe,"Permit one recovery probe after the open circuit's cooldown"),
                    (Choice::NoChange,"Keep the circuit unchanged; abstain or wait for more evidence"),
                ])
            }).build();
            let task = match task {
                Ok(t) => t,
                Err(e) => return Inference::failed("configuration", e.to_string()),
            };
            // A fresh adapter per evaluation makes diagnostics belong to this response.
            let judge = TypeSafeJudge::new(self.client.clone(), task).select_answer(|a| a.action);
            let controller = Controller::builder()
                .judge(judge)
                .inference_timeout(Duration::from_secs(2))
                .build()
                .expect("positive deadline");
            let result = controller.evaluate(&evidence.model_input()).await;
            let mut inference = match result {
                Ok(proposal) => {
                    let diagnostics = controller.judge().last_response();
                    Inference {
                        action: Some(*proposal.action()),
                        confidence: proposal.confidence(),
                        probabilities: diagnostics
                            .as_ref()
                            .map(|d| d.probabilities.clone())
                            .unwrap_or_default(),
                        model: diagnostics.as_ref().map(|d| d.model.clone()),
                        usage: diagnostics.as_ref().map(|d| d.usage.clone()),
                        request_id: diagnostics.and_then(|d| d.request_id),
                        error: None,
                        wall_latency_ms: 0.0,
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
            inference.wall_latency_ms = start.elapsed().as_secs_f64() * 1000.0;
            inference
        })
    }
}

#[derive(Clone)]
struct Data {
    now: f64,
    until: Option<f64>,
    generation: u64,
    revision: u64,
    probe_reserved: bool,
    samples: VecDeque<Observation>,
    datadog: bool,
    changed_wall_ms: u64,
}
#[derive(Clone)]
struct Action {
    choice: Choice,
    telemetry: Option<crate::datadog::TelemetryEvidence>,
    observed_at: f64,
    revision: u64,
}
#[derive(Clone)]
enum Event {
    Clock(f64),
    Response(Observation),
    ReserveProbe,
}
fn valid_phase(phase: &CircuitPhase, d: &Data) -> Result<(), Rejection> {
    if (*phase == CircuitPhase::Open) == d.until.is_some()
        && (!d.probe_reserved || *phase == CircuitPhase::HalfOpen)
    {
        Ok(())
    } else {
        Err(Rejection::new(
            "phase_data",
            "phase, cooldown, and probe reservation must agree",
        ))
    }
}
fn fresh(d: &Data, a: &Action, _: Instant) -> Result<(), Rejection> {
    if a.revision != d.revision {
        return Err(Rejection::new(
            "stale_revision",
            "circuit changed while inference was pending",
        ));
    }
    if d.datadog {
        let telemetry = a
            .telemetry
            .as_ref()
            .ok_or_else(|| Rejection::new("missing_telemetry", "Datadog evidence is required"))?;
        telemetry
            .validate(crate::datadog::unix_ms())
            .map_err(|reason| Rejection::new("stale_telemetry", reason))?;
    }
    let limit = if d.datadog { 10_000.0 } else { 5000.0 };
    if d.now < a.observed_at || d.now - a.observed_at > limit {
        return Err(Rejection::new(
            "stale_evidence",
            "recommendation exceeded its execution deadline",
        ));
    }
    Ok(())
}
fn can_open(d: &Data, a: &Action, now: Instant) -> Result<(), Rejection> {
    fresh(d, a, now)?;
    if d.datadog {
        let t = a.telemetry.as_ref().expect("freshness checked");
        if t.short_window.start_unix_ms
            < d.changed_wall_ms
                .saturating_add(crate::datadog::TRANSITION_MARGIN_MS)
            || t.short_window.responses < 10.0
        {
            return Err(Rejection::new(
                "insufficient_evidence",
                "Opening requires ten responses from a complete post-transition Datadog window",
            ));
        }
        return Ok(());
    }
    if d.samples
        .iter()
        .filter(|s| s.at_ms > d.now - 5000.0)
        .count()
        < 10
    {
        return Err(Rejection::new(
            "insufficient_evidence",
            "at least 10 recent responses are required to open",
        ));
    }
    Ok(())
}
fn can_probe(d: &Data, a: &Action, now: Instant) -> Result<(), Rejection> {
    fresh(d, a, now)?;
    if !d.until.is_some_and(|until| d.now >= until) {
        return Err(Rejection::new(
            "cooldown",
            "the 3-second cooldown has not elapsed",
        ));
    }
    Ok(())
}
fn open(d: &mut Data, _: &Action, _: Instant) -> Result<(), Rejection> {
    start_cooldown(d);
    Ok(())
}
fn start_cooldown(d: &mut Data) {
    d.until = Some(d.now + 3000.0);
    d.generation += 1;
    d.revision += 1;
    d.probe_reserved = false;
    d.samples.clear();
    if d.datadog {
        d.changed_wall_ms = crate::datadog::unix_ms();
    }
}
fn probe(d: &mut Data, _: &Action, _: Instant) -> Result<(), Rejection> {
    d.until = None;
    d.revision += 1;
    d.probe_reserved = false;
    Ok(())
}
fn clock(d: &mut Data, e: &Event, _: Instant) -> Result<(), Rejection> {
    if let Event::Clock(at) = e {
        d.now = *at;
    }
    Ok(())
}
fn current_response(d: &Data, e: &Event, _: Instant) -> Result<(), Rejection> {
    if matches!(e,Event::Response(o) if o.generation==d.generation) {
        Ok(())
    } else {
        Err(Rejection::new(
            "old_generation",
            "response belongs to a prior circuit generation",
        ))
    }
}
fn record(d: &mut Data, e: &Event, _: Instant) -> Result<(), Rejection> {
    if d.datadog {
        return Ok(());
    }
    if let Event::Response(o) = e {
        d.samples.retain(|s| s.at_ms > o.at_ms - 5000.0);
        d.samples.push_back(*o);
    }
    Ok(())
}
fn reserve(d: &mut Data, _: &Event, _: Instant) -> Result<(), Rejection> {
    d.probe_reserved = true;
    Ok(())
}
fn unreserved(d: &Data, _: &Event, _: Instant) -> Result<(), Rejection> {
    if !d.probe_reserved {
        Ok(())
    } else {
        Err(Rejection::new(
            "probe_budget",
            "one probe is already in flight",
        ))
    }
}
fn close(d: &mut Data, _: &Event, _: Instant) -> Result<(), Rejection> {
    d.until = None;
    d.revision += 1;
    d.probe_reserved = false;
    d.samples.clear();
    if d.datadog {
        d.changed_wall_ms = crate::datadog::unix_ms();
    }
    Ok(())
}
fn reopen(d: &mut Data, _: &Event, _: Instant) -> Result<(), Rejection> {
    start_cooldown(d);
    Ok(())
}

pub struct JevPolicy {
    machine: StateMachineExecutor<CircuitPhase, Data, Action, Event>,
    phase: CircuitPhase,
}
impl JevPolicy {
    pub fn new() -> Result<Self, Error> {
        Self::configured(false)
    }
    pub fn datadog() -> Result<Self, Error> {
        Self::configured(true)
    }
    fn configured(datadog: bool) -> Result<Self, Error> {
        let definition = state_machine! {
            phase:CircuitPhase,data:Data,action:Action,event:Event,invariants:[valid_phase],
            transitions:[
                CircuitPhase::Closed + action(Action{choice:Choice::Open,..}) => CircuitPhase::Open {guard:can_open,update:open},
                CircuitPhase::Open + action(Action{choice:Choice::Probe,..}) => CircuitPhase::HalfOpen {guard:can_probe,update:probe},
                CircuitPhase::Closed + action(Action{choice:Choice::NoChange,..}) => CircuitPhase::Closed {guard:fresh},
                CircuitPhase::Open + action(Action{choice:Choice::NoChange,..}) => CircuitPhase::Open {guard:fresh},
                CircuitPhase::HalfOpen + action(Action{choice:Choice::NoChange,..}) => CircuitPhase::HalfOpen {guard:fresh},
                CircuitPhase::Closed + event(Event::Clock(_)) => CircuitPhase::Closed {update:clock},
                CircuitPhase::Open + event(Event::Clock(_)) => CircuitPhase::Open {update:clock},
                CircuitPhase::HalfOpen + event(Event::Clock(_)) => CircuitPhase::HalfOpen {update:clock},
                CircuitPhase::Closed + event(Event::Response(_)) => CircuitPhase::Closed {guard:current_response,update:record},
                CircuitPhase::Open + event(Event::Response(_)) => unchanged {},
                CircuitPhase::HalfOpen + event(Event::ReserveProbe) => CircuitPhase::HalfOpen {guard:unreserved,update:reserve},
                CircuitPhase::HalfOpen + event(Event::Response(Observation{outcome:ClientOutcome::Success,..})) => CircuitPhase::Closed {guard:current_response,update:close},
                CircuitPhase::HalfOpen + event(Event::Response(Observation{outcome:ClientOutcome::Error|ClientOutcome::Timeout,..})) => CircuitPhase::Open {guard:current_response,update:reopen},
                CircuitPhase::Closed + evaluation_error(_) => unchanged {},
                CircuitPhase::Open + evaluation_error(_) => unchanged {},
                CircuitPhase::HalfOpen + evaluation_error(_) => unchanged {},
            ],
        };
        let machine = StateMachineExecutor::builder(definition)
            .store(InMemory::new(
                CircuitPhase::Closed,
                Data {
                    now: 0.0,
                    until: None,
                    generation: 0,
                    revision: 0,
                    probe_reserved: false,
                    samples: VecDeque::new(),
                    datadog,
                    changed_wall_ms: if datadog {
                        crate::datadog::unix_ms()
                    } else {
                        0
                    },
                },
            ))
            .build()
            .map_err(|e| Error::Policy(e.to_string()))?;
        Ok(Self {
            machine,
            phase: CircuitPhase::Closed,
        })
    }
    async fn clock(&mut self, at: f64) -> Result<(), Error> {
        self.machine
            .handle_event(Event::Clock(at))
            .await
            .map_err(|e| Error::Policy(e.to_string()))?;
        Ok(())
    }
    fn data(&self) -> Result<Data, Error> {
        self.machine
            .state()
            .map(|(_, d)| d)
            .map_err(|e| Error::Policy(e.to_string()))
    }
    fn refresh(&mut self) -> Result<(), Error> {
        self.phase = self
            .machine
            .state()
            .map_err(|e| Error::Policy(e.to_string()))?
            .0;
        Ok(())
    }
}
fn window(d: &Data, at: f64, ms: f64) -> Window {
    let samples: Vec<_> = d.samples.iter().filter(|s| s.at_ms > at - ms).collect();
    let mut latencies: Vec<_> = samples.iter().map(|s| s.latency_ms).collect();
    latencies.sort_by(f64::total_cmp);
    let responses = samples.len();
    let successes = samples
        .iter()
        .filter(|s| s.outcome == ClientOutcome::Success)
        .count();
    Window {
        responses,
        successes,
        errors: samples
            .iter()
            .filter(|s| s.outcome == ClientOutcome::Error)
            .count(),
        timeouts: samples
            .iter()
            .filter(|s| s.outcome == ClientOutcome::Timeout)
            .count(),
        failure_ratio: if responses == 0 {
            0.0
        } else {
            (responses - successes) as f64 / responses as f64
        },
        p95_latency_ms: if responses == 0 {
            None
        } else {
            Some(latencies[(responses as f64 * 0.95).ceil() as usize - 1])
        },
    }
}
impl Policy for JevPolicy {
    fn phase(&self) -> CircuitPhase {
        self.phase
    }
    fn evidence(&self, at_ms: f64) -> Option<Evidence> {
        let d = self.data().ok()?;
        let recent = window(&d, at_ms, 5000.0);
        let cooldown = d.until.map_or(0.0, |t| (t - at_ms).max(0.0));
        let mut legal = vec![Choice::NoChange];
        if self.phase == CircuitPhase::Closed && (d.datadog || recent.responses >= 10) {
            legal.push(Choice::Open);
        }
        if self.phase == CircuitPhase::Open && cooldown == 0.0 {
            legal.push(Choice::Probe);
        }
        Some(Evidence {
            forecast: None,
            service: String::new(),
            telemetry: None,
            control_since_unix_ms: d.changed_wall_ms,
            observed_at_ms: at_ms,
            phase: self.phase,
            revision: d.revision,
            last_5_seconds: recent,
            last_1_second: window(&d, at_ms, 1000.0),
            cooldown_remaining_ms: cooldown,
            client_timeout_ms: 650.0,
            legal_actions: legal,
        })
    }
    fn admit(&mut self, c: AdmissionContext) -> PolicyFuture<'_, Admission> {
        Box::pin(async move {
            let d = self.data()?;
            match self.phase {
                CircuitPhase::Closed => Ok(Admission::Allow {
                    generation: d.generation,
                    probe: false,
                }),
                CircuitPhase::HalfOpen if !d.probe_reserved => {
                    let outcome = self
                        .machine
                        .handle_event(Event::ReserveProbe)
                        .await
                        .map_err(|e| Error::Policy(e.to_string()))?;
                    if matches!(outcome, ExecutionOutcome::Applied(_)) {
                        Ok(Admission::Allow {
                            generation: d.generation,
                            probe: true,
                        })
                    } else {
                        Ok(Admission::Shed)
                    }
                }
                _ => {
                    let _ = c;
                    Ok(Admission::Shed)
                }
            }
        })
    }
    fn observe(&mut self, o: Observation) -> PolicyFuture<'_, ()> {
        Box::pin(async move {
            self.clock(o.at_ms).await?;
            self.machine
                .handle_event(Event::Response(o))
                .await
                .map_err(|e| Error::Policy(e.to_string()))?;
            self.refresh()
        })
    }
    fn apply_model(
        &mut self,
        at_ms: f64,
        evidence: Evidence,
        inference: Inference,
    ) -> PolicyFuture<'_, GuardResult> {
        Box::pin(async move {
            self.clock(at_ms).await?;
            let from = self.phase;
            let evaluation = if let Some(error) = &inference.error {
                Err(EvaluationError::Judge(reflex::JudgeError::new(
                    &error.code,
                    &error.message,
                )))
            } else if let Some(choice) = inference.action {
                Judgment {
                    action: Action {
                        choice,
                        telemetry: evidence.telemetry.clone(),
                        observed_at: evidence.observed_at_ms,
                        revision: evidence.revision,
                    },
                    confidence: inference.confidence,
                }
                .try_into()
            } else {
                Err(EvaluationError::Judge(reflex::JudgeError::new(
                    "missing_action",
                    "provider supplied neither an action nor an error",
                )))
            };
            let outcome = self
                .machine
                .execute(evaluation)
                .await
                .map_err(|e| Error::Policy(e.to_string()))?;
            self.refresh()?;
            let (status, reason) = match outcome {
                ExecutionOutcome::Applied(r) if r.evaluation_error.is_some() => (
                    "evaluation_error".into(),
                    "Evaluation failed; executor retained the current circuit state".into(),
                ),
                ExecutionOutcome::Applied(_) if from == self.phase => (
                    "unchanged".into(),
                    "Fresh NoChange recommendation accepted; circuit retained".into(),
                ),
                ExecutionOutcome::Applied(_) => (
                    "applied".into(),
                    "Legality, evidence freshness, and resource guards passed".into(),
                ),
                ExecutionOutcome::Rejected { reason, .. } => (
                    "rejected".into(),
                    format!("{}: {}", reason.code, reason.message),
                ),
            };
            Ok(GuardResult {
                status,
                reason,
                from,
                to: self.phase,
            })
        })
    }
}

#[cfg(test)]
mod datadog_guards {
    use super::*;
    use crate::datadog::{Gauge, Server, TelemetryEvidence, Window as RemoteWindow};
    #[test]
    fn remote_open_uses_remote_samples_and_rechecks_age_revision_and_transition_boundary() {
        let now = crate::datadog::unix_ms();
        let w = RemoteWindow {
            start_unix_ms: now - 50_000,
            end_unix_ms: now - 20_000,
            duration_seconds: 30.,
            responses: 20.,
            successes: 5.,
            http_errors: 5.,
            timeouts: 10.,
            blocked_requests: 100.,
            failure_ratio: Some(0.75),
            p95_latency_ms: None,
        };
        let g = Gauge {
            value: 0.,
            observed_at_unix_ms: now - 20_000,
        };
        let t = TelemetryEvidence {
            source: "datadog".into(),
            status: "usable".into(),
            simulation_run: "run".into(),
            fetched_at_unix_ms: now,
            latest_data_age_seconds: 20.,
            short_window: w.clone(),
            long_window: w,
            server: Server {
                queue_depth: g.clone(),
                active_requests: g.clone(),
                utilization: g.clone(),
                timed_out_work: g.clone(),
                client_in_flight: g,
            },
            latency_status: "unavailable".into(),
        };
        let mut d = Data {
            now: 1000.,
            until: None,
            generation: 0,
            revision: 7,
            probe_reserved: false,
            samples: VecDeque::new(),
            datadog: true,
            changed_wall_ms: now - 90_000,
        };
        let a = Action {
            choice: Choice::Open,
            observed_at: 1000.,
            revision: 7,
            telemetry: Some(t),
        };
        assert!(
            can_open(&d, &a, Instant::now()).is_ok(),
            "No local samples are required in Datadog mode"
        );
        let mut bad = a.clone();
        bad.telemetry = None;
        assert!(can_open(&d, &bad, Instant::now()).is_err());
        let mut bad = a.clone();
        bad.telemetry.as_mut().unwrap().fetched_at_unix_ms = now - 11_000;
        assert!(can_open(&d, &bad, Instant::now()).is_err());
        let mut bad = a.clone();
        bad.telemetry.as_mut().unwrap().short_window.responses = 0.;
        assert!(can_open(&d, &bad, Instant::now()).is_err());
        d.revision += 1;
        assert!(can_open(&d, &a, Instant::now()).is_err());
        d.revision -= 1;
        d.changed_wall_ms = now - 40_000;
        assert!(can_open(&d, &a, Instant::now()).is_err());
        d.changed_wall_ms = now - 90_000;
        d.datadog = false;
        assert!(
            can_open(&d, &a, Instant::now()).is_err(),
            "Local mode still requires local samples"
        );
    }
    #[tokio::test]
    async fn remote_policy_does_not_collect_local_health_samples() {
        let mut policy = JevPolicy::datadog().unwrap();
        for i in 0..20 {
            policy
                .observe(Observation {
                    at_ms: i as f64,
                    request_id: i,
                    outcome: ClientOutcome::Timeout,
                    latency_ms: 650.,
                    generation: 0,
                })
                .await
                .unwrap();
        }
        assert!(policy.data().unwrap().samples.is_empty());
        assert_eq!(policy.evidence(20.).unwrap().last_5_seconds.responses, 0);
    }
}
