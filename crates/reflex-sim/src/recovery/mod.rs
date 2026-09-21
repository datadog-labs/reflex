//! Read-only replica recovery with resource competition and asynchronous judgment.
pub mod datadog;
pub mod engine;
pub mod judge;
mod telemetry;
pub mod web;
use crate::decision_trace::DecisionTrace;
use crate::{
    playground::inference::{CostStatus, JevSettings},
    Error,
};
use engine::{Action, ClientConfig, Engine, Event, Faults, Proposal, HORIZON, STEP};
use judge::{Evaluator, Evidence, Inference};
use opentelemetry::metrics::Meter;
use serde::{Deserialize, Serialize};
use std::{
    sync::{Arc, Mutex},
    time::Instant,
};
use tokio::task::JoinHandle;
use tracing::Instrument;
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Policy {
    Jev,
    Fixed,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scenario {
    Sandbox,
    SingleCrash,
    RebuildPressure,
    SecondFailure,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Command {
    Forecast { enabled: bool },
    Play,
    Pause,
    Step,
    Reset,
    Speed { value: u8 },
    Policy { policy: Policy },
    Scenario { scenario: Scenario },
    Fault { replica: usize, faults: Faults },
    Client { id: u64, config: ClientConfig },
    AddClient,
    RemoveClient { id: u64 },
    Bandwidth { value: f64 },
}
#[derive(Clone, Serialize)]
pub struct Decision {
    pub id: usize,
    pub at_ms: u64,
    pub policy: Policy,
    pub action: Option<Action>,
    pub label: String,
    pub status: String,
    pub reason: String,
    pub evidence: Evidence,
    pub result: Option<Inference>,
}
#[derive(Clone, Serialize)]
pub struct Sample {
    pub at_ms: u64,
    pub essential_success: f64,
    pub ready: usize,
    pub in_flight: usize,
    pub recovery_progress: f64,
}
#[derive(Serialize)]
pub struct View {
    pub horizon_ms: u64,
    pub forecast: crate::forecasting::View,
    #[serde(flatten)]
    pub data: engine::Data,
    pub paused: bool,
    pub speed: u8,
    pub policy: Policy,
    pub scenario: Scenario,
    pub available: bool,
    pub pending: bool,
    pub response_ready: bool,
    pub calls: usize,
    pub call_limit: usize,
    pub cost: CostStatus,
    pub evidence: Evidence,
    pub decisions: Vec<Decision>,
    pub history: Vec<Sample>,
    pub status: String,
    pub evidence_source: String,
    pub simulation_run: Option<String>,
    pub telemetry_status: Option<String>,
    pub error: Option<String>,
}
struct Pending {
    trace: DecisionTrace,
    evidence: Evidence,
    task: JoinHandle<Inference>,
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
    forecast_arrivals: u64,
    pub(crate) forecast: crate::forecasting::Driver,
    source: Option<Arc<crate::datadog::Source>>,
    run: Option<String>,
    not_before: u64,
    fetch: Option<Fetch>,
    last_fetch: Option<Instant>,
    cached: Option<datadog::TelemetryEvidence>,
    telemetry_status: String,
    meter: Meter,
    engine: Engine,
    seed: u64,
    paused: bool,
    speed: u8,
    policy: Policy,
    scenario: Scenario,
    evaluator: Option<Arc<dyn Evaluator>>,
    settings: JevSettings,
    pending: Option<Pending>,
    last_dispatch: Option<Instant>,
    calls: usize,
    cost: Arc<Mutex<CostStatus>>,
    decisions: Vec<Decision>,
    history: Vec<Sample>,
    edits: Vec<serde_json::Value>,
    remainder: u64,
    last_baseline: u64,
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
            opentelemetry::global::meter("recovery"),
        )
    }
    pub fn with_meter(
        seed: u64,
        evaluator: Option<Arc<dyn Evaluator>>,
        settings: JevSettings,
        meter: Meter,
    ) -> Result<Self, Error> {
        let policy = if evaluator.is_some() {
            Policy::Jev
        } else {
            Policy::Fixed
        };
        let source = evaluator.as_ref().and_then(|e| e.evidence_source());
        let run = source.as_ref().map(|_| datadog::new_run());
        Ok(Self {
            engine: Engine::with_run(seed, policy, meter.clone(), run.clone())?,
            source,
            run,
            not_before: crate::datadog::unix_ms(),
            fetch: None,
            last_fetch: None,
            cached: None,
            telemetry_status: "Waiting for Datadog observations".into(),
            meter,
            seed,
            paused: true,
            speed: 1,
            policy,
            scenario: Scenario::Sandbox,
            evaluator,
            settings,
            forecast_arrivals: 0,
            forecast: Default::default(),
            pending: None,
            last_dispatch: None,
            calls: 0,
            cost: Arc::new(Mutex::new(CostStatus::default())),
            decisions: vec![],
            history: vec![],
            edits: vec![],
            remainder: 0,
            last_baseline: 0,
            error: None,
        })
    }
    fn horizon(&self) -> u64 {
        if self.uses_datadog() {
            600_000
        } else {
            HORIZON
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
        self.telemetry_status = "Warming up: waiting for post-resume Datadog observations".into();
    }
    async fn poll_evidence(&mut self) {
        if self.fetch.as_ref().is_some_and(|f| f.task.is_finished()) {
            let mut f = self.fetch.take().unwrap();
            match (&mut f.task).await {
                Ok(Ok(e)) => {
                    self.cached = Some(e);
                    self.telemetry_status = "Datadog evidence ready".into();
                }
                Ok(Err(reason)) => {
                    self.cached = None;
                    self.telemetry_status = reason;
                }
                Err(_) => {
                    self.cached = None;
                    self.telemetry_status =
                        "Datadog evidence task failed; current plan retained".into();
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
        let boundary = self.not_before;
        self.last_fetch = Some(Instant::now());
        self.fetch = Some(Fetch {
            task: tokio::spawn(async move { source.fetch_recovery(&run, boundary).await }),
        });
    }
    pub fn data(&self) -> engine::Data {
        self.engine.data()
    }
    pub fn view(&self) -> View {
        let d = self.data();
        let e = judge::evidence(&d);
        let status = if d.at_ms >= self.horizon() {
            "Run complete · reset to start again"
        } else if self.paused {
            "Paused · edit clients or inject a failure, then resume"
        } else if self.pending.is_some() {
            "Evaluating observed health · reads and recovery continue"
        } else if self.policy == Policy::Jev && self.calls >= self.settings.max_evaluations {
            "Evaluation budget exhausted · current plan continues"
        } else if d.intervention {
            "Operator intervention requested · inspect the decision journal"
        } else {
            "Observing health · enforcing the current recovery plan"
        };
        View {
            horizon_ms: self.horizon(),
            forecast: self.forecast.view(d.at_ms),
            data: d,
            paused: self.paused,
            speed: self.speed,
            policy: self.policy,
            scenario: self.scenario,
            available: self.evaluator.is_some(),
            pending: self.pending.is_some(),
            response_ready: self.pending.as_ref().is_some_and(|p| p.task.is_finished()),
            calls: self.calls,
            call_limit: self.settings.max_evaluations,
            cost: self.cost.lock().unwrap().clone(),
            evidence: e,
            decisions: self.decisions.iter().rev().take(30).cloned().collect(),
            history: self.history.clone(),
            status: status.into(),
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
        serde_json::json!({"model":"reflex-recovery-v1","view":self.view(),"decisions":self.decisions,"controls":self.edits})
    }
    async fn reset(&mut self, policy: Policy) -> Result<(), Error> {
        self.forecast_arrivals = 0;
        self.forecast.reset();
        let mut clients = self.data().clients;
        let bandwidth = if self.scenario == Scenario::Sandbox {
            self.data().bandwidth_limit
        } else {
            12.
        };
        self.pending = None;
        if self.uses_datadog() {
            self.run = Some(datadog::new_run());
            self.clear_evidence();
        }
        self.engine = Engine::with_run(self.seed, policy, self.meter.clone(), self.run.clone())?;
        self.policy = policy;
        if self.scenario != Scenario::Sandbox {
            clients = self.data().clients;
        }
        while self.data().clients.len() < clients.len() {
            self.engine.event(Event::AddClient).await?;
        }
        while self.data().clients.len() > clients.len() {
            let id = self.data().clients.last().unwrap().id;
            self.engine.event(Event::RemoveClient(id)).await?;
        }
        for (i, c) in clients.into_iter().enumerate() {
            self.engine
                .event(Event::Client {
                    id: i as u64,
                    config: c.config,
                })
                .await?;
        }
        self.engine.event(Event::Bandwidth(bandwidth)).await?;
        self.paused = true;
        self.remainder = 0;
        self.last_baseline = 0;
        self.calls = 0;
        self.last_dispatch = None;
        self.decisions.clear();
        self.history.clear();
        self.edits.clear();
        self.error = None;
        Ok(())
    }
    pub async fn command(&mut self, c: Command) -> Result<(), Error> {
        let edit = serde_json::json!({"at_ms":self.data().at_ms,"command":c});
        if self.uses_datadog()
            && matches!(
                &c,
                Command::Step
                    | Command::Speed { value: 2.. }
                    | Command::Policy {
                        policy: Policy::Fixed
                    }
            )
        {
            return Err(Error::Invalid(
                "Datadog evidence requires Jev and continuous 1× playback".into(),
            ));
        }
        match c {
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
                    return Err(Error::Invalid("Reset before continuing".into()));
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
            Command::Reset => self.reset(self.policy).await?,
            Command::Speed { value } => {
                if ![1, 2, 4].contains(&value) {
                    return Err(Error::Invalid("Speed must be 1, 2 or 4".into()));
                }
                self.speed = value;
            }
            Command::Policy { policy } => {
                if policy == Policy::Jev && self.evaluator.is_none() {
                    return Err(Error::Invalid("Set TYPESAFE_API_KEY to enable Jev".into()));
                }
                self.reset(policy).await?;
            }
            Command::Scenario { scenario } => {
                self.scenario = scenario;
                self.reset(self.policy).await?;
            }
            Command::Fault { replica, faults } => {
                self.engine.event(Event::Fault { replica, faults }).await?
            }
            Command::Client { id, config } => {
                self.engine.event(Event::Client { id, config }).await?
            }
            Command::AddClient => self.engine.event(Event::AddClient).await?,
            Command::RemoveClient { id } => self.engine.event(Event::RemoveClient(id)).await?,
            Command::Bandwidth { value } => self.engine.event(Event::Bandwidth(value)).await?,
        }
        self.edits.push(edit);
        Ok(())
    }
    async fn update_forecast(&mut self) {
        let d = self.data();
        if d.at_ms.is_multiple_of(1000) {
            let arrivals = d.totals.arrivals.saturating_sub(self.forecast_arrivals);
            self.forecast_arrivals = d.totals.arrivals;
            let failures = d
                .outcomes
                .iter()
                .filter(|o| o.at_ms > d.at_ms.saturating_sub(1000) && !o.success && !o.rejected)
                .count();
            let queued = d
                .replicas
                .iter()
                .map(|n| {
                    d.requests
                        .iter()
                        .filter(|r| r.node == n.id)
                        .count()
                        .saturating_sub(4)
                })
                .sum::<usize>();
            self.forecast.sample(
                d.at_ms,
                [arrivals as f32, failures as f32, queued as f32],
                [
                    "offered_requests_per_second",
                    "failed_responses_per_second",
                    "replica_queue_depth",
                ],
            );
        }
        let dd = self
            .source
            .clone()
            .zip(self.run.clone())
            .map(|(s, r)| (s, r, self.not_before, "recovery"));
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
    async fn scripted(&mut self) -> Result<(), Error> {
        let d = self.data();
        let t = d.at_ms;
        let change = match (self.scenario, t) {
            (
                Scenario::SingleCrash | Scenario::RebuildPressure | Scenario::SecondFailure,
                10_000,
            ) => Some((0, true)),
            (Scenario::SecondFailure, 18_000) => Some((1, true)),
            (
                Scenario::SingleCrash | Scenario::RebuildPressure | Scenario::SecondFailure,
                65_000,
            ) => Some((0, false)),
            (Scenario::SecondFailure, 80_000) => Some((1, false)),
            _ => None,
        };
        if let Some((replica, crashed)) = change {
            let mut faults = d.replicas[replica].faults.clone();
            faults.crashed = crashed;
            self.engine.event(Event::Fault { replica, faults }).await?;
        }
        if self.scenario == Scenario::RebuildPressure && (t == 16_000 || t == 50_000) {
            for c in d.clients {
                let mut config = c.config;
                config.rate = if t == 16_000 { 25. } else { 8. };
                config.cost_ms = if t == 16_000 { 300 } else { 150 };
                self.engine
                    .event(Event::Client { id: c.id, config })
                    .await?;
            }
        }
        Ok(())
    }
    async fn advance(&mut self, ms: u64) -> Result<(), Error> {
        self.remainder += ms;
        while self.remainder >= STEP && self.data().at_ms < self.horizon() {
            self.remainder -= STEP;
            self.engine.event(Event::Tick).await?;
            self.scripted().await?;
            let d = self.data();
            if self.policy == Policy::Fixed && d.at_ms >= self.last_baseline + 1000 {
                self.last_baseline = d.at_ms;
                let e = judge::evidence(&d);
                let a = judge::baseline(&e);
                self.apply(e, Some(a), None).await?;
            }
            if d.at_ms.is_multiple_of(500) {
                let e = judge::evidence(&d);
                self.history.push(Sample {
                    at_ms: d.at_ms,
                    essential_success: e.long_window.essential_success_rate,
                    ready: d
                        .replicas
                        .iter()
                        .filter(|n| n.phase == engine::Lifecycle::Ready)
                        .count(),
                    in_flight: d.requests.len(),
                    recovery_progress: d.recovery.as_ref().map_or(0., |r| r.progress_mb),
                });
            }
        }
        if self.data().at_ms >= self.horizon() {
            self.paused = true;
            if self.uses_datadog() {
                self.clear_evidence();
            }
        }
        Ok(())
    }
    async fn apply(
        &mut self,
        e: Evidence,
        action: Option<Action>,
        result: Option<Inference>,
    ) -> Result<(), Error> {
        let mut trace = DecisionTrace::recovery(
            self.decisions.len() as u64 + 1,
            telemetry::policy(self.policy),
            self.data().at_ms,
        );
        self.apply_traced(e, action, result, &mut trace).await
    }
    async fn apply_traced(
        &mut self,
        e: Evidence,
        action: Option<Action>,
        result: Option<Inference>,
        trace: &mut DecisionTrace,
    ) -> Result<(), Error> {
        let child = trace.child("apply");
        let result = trace
            .scope(self.apply_inner(e, action, result).instrument(child))
            .await;
        let status = if result.is_err() {
            "error"
        } else {
            &self.decisions.last().unwrap().status
        };
        trace.finish(status);
        result
    }
    async fn apply_inner(
        &mut self,
        e: Evidence,
        action: Option<Action>,
        result: Option<Inference>,
    ) -> Result<(), Error> {
        let invalid = if self.uses_datadog() {
            e.telemetry
                .as_ref()
                .ok_or_else(|| "Missing Datadog recovery evidence".to_owned())
                .and_then(|t| {
                    t.validate(
                        self.run.as_deref().unwrap(),
                        self.not_before,
                        crate::datadog::unix_ms(),
                    )
                })
                .err()
        } else {
            None
        };
        let (status, reason): (String, String) = if let Some(reason) = invalid {
            ("rejected".into(), reason)
        } else if let Some(a) = action {
            let (ok, reason) = self
                .engine
                .apply(
                    Proposal {
                        action: a,
                        observed_at: e.observed_at_ms,
                        revision: e.revision,
                    },
                    result.as_ref().and_then(|r| r.confidence),
                )
                .await?;
            (
                if ok {
                    if a == Action::KeepCurrentPlan {
                        "unchanged"
                    } else {
                        "applied"
                    }
                } else {
                    "rejected"
                }
                .into(),
                reason,
            )
        } else {
            let error = result
                .as_ref()
                .and_then(|r| r.error.as_deref())
                .unwrap_or("No recommendation");
            self.engine.evaluation_error(error).await?;
            (
                "evaluation_error".into(),
                format!("{error}; existing plan retained"),
            )
        };
        tracing::info!(target: "reflex_sim::recovery", policy = telemetry::policy(self.policy), action = ?action, status, "Recovery decision handled");
        self.decisions.push(Decision {
            id: self.decisions.len() + 1,
            at_ms: self.data().at_ms,
            policy: self.policy,
            action,
            label: action.map_or_else(|| "Evaluation failed".into(), |a| a.label()),
            status,
            reason,
            evidence: e,
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
            let result = match (&mut p.task).await {
                Ok(r) => r,
                Err(_) => Inference::failed("Inference worker failed"),
            };
            self.apply_traced(
                p.evidence.clone(),
                result.choice,
                Some(result),
                &mut p.trace,
            )
            .await?;
        }
        if self.pending.is_some()
            || self.calls >= self.settings.max_evaluations
            || self.data().at_ms >= self.horizon()
            || self
                .last_dispatch
                .is_some_and(|t| t.elapsed() < self.settings.dispatch_interval)
        {
            return Ok(());
        }
        let Some(evaluator) = self.evaluator.clone() else {
            return Ok(());
        };
        let mut evidence = judge::evidence(&self.data());
        if self.uses_datadog() {
            let Some(t) = self.cached.clone() else {
                return Ok(());
            };
            if let Err(reason) = t.validate(
                self.run.as_deref().unwrap(),
                self.not_before,
                crate::datadog::unix_ms(),
            ) {
                self.telemetry_status = reason;
                return Ok(());
            }
            self.telemetry_status = format!(
                "Datadog window ends {:.0}s ago · p95 {}",
                (crate::datadog::unix_ms() - t.short_window.end_unix_ms) as f64 / 1000.,
                t.latency_status
            );
            evidence.telemetry = Some(t);
        }
        evidence.forecast = self.forecast.evidence(self.data().at_ms);
        let captured = evidence.clone();
        let cost = self.cost.clone();
        self.calls += 1;
        self.last_dispatch = Some(Instant::now());
        cost.lock().unwrap().dispatched();
        let trace = DecisionTrace::recovery(
            self.calls as u64,
            telemetry::policy(self.policy),
            evidence.observed_at_ms,
        );
        let child = trace.child("evaluate");
        let task = tokio::spawn(
            trace.scope(
                async move {
                    let r = evaluator.evaluate(captured).await;
                    cost.lock()
                        .unwrap()
                        .record_usage(r.model.as_deref(), r.usage.as_ref());
                    r
                }
                .instrument(child),
            ),
        );
        self.pending = Some(Pending {
            evidence,
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
                Inference {
                    choice: Some(Action::KeepCurrentPlan),
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
    #[tokio::test]
    async fn remote_input_excludes_local_health_and_retains_guards_and_lifecycle() {
        use opentelemetry::metrics::MeterProvider;
        let cap = metric_capture::Capture::new();
        let captured = Arc::new(Mutex::new(vec![]));
        let evaluator = Arc::new(datadog::DatadogEvaluator::new(
            Arc::new(Judge(captured.clone())),
            crate::datadog::Source::for_test("http://127.0.0.1:1"),
        ));
        let mut s = Session::with_meter(
            42,
            Some(evaluator),
            JevSettings::default(),
            cap.provider.meter("recovery"),
        )
        .unwrap();
        s.command(Command::Play).await.unwrap();
        s.advance(4000).await.unwrap();
        s.last_fetch = Some(Instant::now());
        s.infer().await.unwrap();
        assert_eq!(s.calls, 0);
        s.not_before = crate::datadog::unix_ms() - 120_000;
        s.cached = Some(datadog::test_evidence(
            s.run.as_deref().unwrap(),
            crate::datadog::unix_ms(),
        ));
        s.infer().await.unwrap();
        for _ in 0..20 {
            tokio::task::yield_now().await;
            if s.pending.as_ref().is_some_and(|p| p.task.is_finished()) {
                break;
            }
        }
        s.infer().await.unwrap();
        assert_eq!(s.calls, 1);
        let input = captured.lock().unwrap()[0].clone();
        assert_eq!(input.as_object().unwrap().len(), 2);
        assert_eq!(input["telemetry"]["source"], "datadog");
        for key in [
            "short_window",
            "long_window",
            "reachable",
            "heartbeat_age_ms",
            "in_flight",
            "responses_5s",
        ] {
            assert!(input.get(key).is_none());
            assert!(input["control"]["replicas"][0].get(key).is_none());
        }
        let mut evidence = judge::evidence(&s.data());
        evidence.telemetry = s.cached.clone();
        s.command(Command::Bandwidth { value: 4. }).await.unwrap();
        s.apply(evidence, Some(Action::KeepCurrentPlan), None)
            .await
            .unwrap();
        assert_eq!(s.decisions.last().unwrap().status, "rejected");
        let mut evidence = judge::evidence(&s.data());
        evidence.telemetry = s.cached.clone();
        evidence.telemetry.as_mut().unwrap().fetched_at_unix_ms -= 20_000;
        s.apply(
            evidence,
            Some(Action::SetServingMode {
                essential_only: true,
            }),
            None,
        )
        .await
        .unwrap();
        assert!(!s.data().essential_only);
        assert!(s.command(Command::Step).await.is_err());
        assert!(s.command(Command::Speed { value: 4 }).await.is_err());
        assert!(s
            .command(Command::Policy {
                policy: Policy::Fixed
            })
            .await
            .is_err());
        let run = s.run.clone().unwrap();
        let m = cap.read();
        assert!(
            metric_capture::count(
                &m,
                "http.client.requests",
                &[("simulation_run", &run), ("replica", "replica_a")]
            ) > 0
        );
        assert_eq!(
            metric_capture::gauge(
                &m,
                "recovery.replica.requests.outstanding",
                &[("simulation_run", &run)]
            )
            .len(),
            4
        );
        s.command(Command::Pause).await.unwrap();
        assert!(s.cached.is_none());
        assert!(s.pending.is_none());
        assert!(s.fetch.is_none());
        s.command(Command::Reset).await.unwrap();
        assert_ne!(s.run.as_deref(), Some(run.as_str()));
        let m = cap.read();
        assert!(metric_capture::gauge(
            &m,
            "recovery.replica.heartbeat.age",
            &[("simulation_run", &run)]
        )
        .is_empty());
    }
}
