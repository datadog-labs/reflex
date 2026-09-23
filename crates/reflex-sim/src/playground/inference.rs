// Unless explicitly stated otherwise all files in this repository are licensed under the Apache-2.0 License.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-present Datadog, Inc.

use crate::decision_trace::DecisionTrace;
use crate::{
    engine::live::LiveSimulation,
    jev::{Evaluator, Evidence, Inference, PolicyKind},
    Error,
};
use serde::Serialize;
use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};
use tokio::task::JoinHandle;
use tracing::Instrument;

#[derive(Clone)]
pub struct JevSettings {
    pub model: String,
    pub max_evaluations: usize,
    pub dispatch_interval: Duration,
}
impl Default for JevSettings {
    fn default() -> Self {
        Self {
            model: "jev-1.13.0".into(),
            max_evaluations: 180,
            dispatch_interval: Duration::from_secs(1),
        }
    }
}
#[derive(Serialize)]
pub struct InferenceStatus {
    pub available: bool,
    pub model: String,
    pub calls: usize,
    pub limit: usize,
    pub budget_exhausted: bool,
    pub pending: Option<PendingStatus>,
    pub cost: CostStatus,
    pub evidence_source: &'static str,
    pub telemetry_status: String,
    pub evidence_age_seconds: Option<f64>,
    pub simulation_run: Option<String>,
}
// Published Jev 1.13 pricing, verified 2026-09-20:
// https://docs.typesafe.ai/models — $0.042 / million input tokens; output free.
const INPUT_USD_PER_MILLION: f64 = 0.042;
#[derive(Clone, Default, Serialize)]
pub struct CostStatus {
    pub estimated_usd: f64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub calls: usize,
    pub priced_calls: usize,
    pub unpriced_calls: usize,
    pub missing_usage_calls: usize,
}
impl CostStatus {
    pub(crate) fn dispatched(&mut self) {
        self.calls += 1;
        self.missing_usage_calls += 1;
    }
    fn record(&mut self, result: &Inference) {
        self.record_usage(result.model.as_deref(), result.usage.as_ref());
    }
    pub(crate) fn record_usage(&mut self, model: Option<&str>, usage: Option<&typesafe_ai::Usage>) {
        let Some(usage) = usage else { return };
        self.missing_usage_calls -= 1;
        self.input_tokens += usage.input_tokens;
        self.output_tokens += usage.output_tokens;
        // Use the resolved model, never assume an alias still has this price.
        if model == Some("jev-1.13.0") {
            self.priced_calls += 1;
            self.estimated_usd += usage.input_tokens as f64 * INPUT_USD_PER_MILLION / 1_000_000.0;
        } else {
            self.unpriced_calls += 1;
        }
    }
}
#[derive(Serialize)]
pub struct PendingStatus {
    pub service: usize,
    pub observed_at_ms: f64,
    pub response_ready: bool,
}
struct Pending {
    id: u64,
    service: usize,
    evidence: Evidence,
    task: JoinHandle<(Evidence, Inference)>,
    trace: Option<DecisionTrace>,
}
impl Drop for Pending {
    fn drop(&mut self) {
        self.task.abort();
    }
}
pub(super) struct CompletedInference {
    pub id: u64,
    pub service: usize,
    pub evidence: Evidence,
    pub inference: Inference,
    pub trace: DecisionTrace,
}
pub struct Driver {
    pub(crate) forecasts: [crate::forecasting::Driver; 3],
    forecast_counts: [(u64, u64); 3],
    pub evaluator: Option<Arc<dyn Evaluator>>,
    pub settings: JevSettings,
    pub(super) cost: Arc<Mutex<CostStatus>>,
    pending: Option<Pending>,
    calls: Arc<AtomicUsize>,
    next_due: [f64; 3],
    last_dispatch: Option<Instant>,
    round_robin: usize,
    run: String,
    active_since: u64,
    attempts: u64,
    telemetry_status: String,
    latest_evidence_at: Option<u64>,
}
static NEXT_RUN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
impl Driver {
    pub fn new(evaluator: Option<Arc<dyn Evaluator>>, settings: JevSettings) -> Self {
        Self {
            forecasts: std::array::from_fn(|_| Default::default()),
            forecast_counts: [(0, 0); 3],
            evaluator,
            settings,
            cost: Arc::new(Mutex::new(CostStatus::default())),
            pending: None,
            calls: Arc::new(AtomicUsize::new(0)),
            next_due: [0.0; 3],
            last_dispatch: None,
            round_robin: 0,
            run: format!(
                "{}-{}-{}",
                crate::datadog::unix_ms(),
                std::process::id(),
                NEXT_RUN.fetch_add(1, Ordering::Relaxed)
            ),
            active_since: crate::datadog::unix_ms(),
            attempts: 0,
            latest_evidence_at: None,
            telemetry_status: "Start traffic to collect a fresh Datadog window".into(),
        }
    }
    pub async fn forecast(&mut self, view: crate::engine::live::LiveView) {
        let now = view.at_ms as u64;
        for (i, service) in view.services.iter().enumerate() {
            let c = &service.state.counts;
            if now.is_multiple_of(1000) {
                let values = [
                    c.offered.saturating_sub(self.forecast_counts[i].0) as f32,
                    (c.error + c.timeout).saturating_sub(self.forecast_counts[i].1) as f32,
                    service.state.queued as f32,
                ];
                self.forecast_counts[i] = (c.offered, c.error + c.timeout);
                self.forecasts[i].sample(
                    now,
                    values,
                    [
                        "offered_requests_per_second",
                        "failed_responses_per_second",
                        "queue_depth",
                    ],
                );
            }
            let dd = self
                .evaluator
                .as_ref()
                .and_then(|e| e.evidence_source())
                .map(|s| {
                    (
                        s,
                        self.run.clone(),
                        self.active_since,
                        service.definition.id.as_str(),
                    )
                });
            self.forecasts[i].poll(now, dd).await;
        }
    }
    pub fn datadog(&self) -> bool {
        self.evaluator
            .as_ref()
            .is_some_and(|e| e.evidence_source().is_some())
    }
    pub fn run(&self) -> &str {
        &self.run
    }
    pub fn resume(&mut self) {
        self.cancel();
        for f in &mut self.forecasts {
            f.reset();
        }
        self.active_since = crate::datadog::unix_ms();
        self.telemetry_status = "Warming up: collecting a complete Datadog window".into();
    }
    pub fn fresh(&self) -> Self {
        let mut next = Self::new(self.evaluator.clone(), self.settings.clone());
        next.cost = self.cost.clone();
        for i in 0..3 {
            next.forecasts[i].provider = self.forecasts[i].provider.clone();
            next.forecasts[i].enabled = self.forecasts[i].enabled;
        }
        next
    }
    pub fn cancel(&mut self) {
        self.pending = None;
    }
    pub fn status(&self) -> InferenceStatus {
        InferenceStatus {
            evidence_source: if self.datadog() { "datadog" } else { "local" },
            simulation_run: self.datadog().then(|| self.run.clone()),
            telemetry_status: self.telemetry_status.clone(),
            evidence_age_seconds: self
                .latest_evidence_at
                .map(|at| crate::datadog::unix_ms().saturating_sub(at) as f64 / 1000.),
            cost: self.cost.lock().unwrap().clone(),
            available: self.evaluator.is_some(),
            model: self.settings.model.clone(),
            calls: self.calls.load(Ordering::Relaxed),
            limit: self.settings.max_evaluations,
            budget_exhausted: self.calls.load(Ordering::Relaxed) >= self.settings.max_evaluations,
            pending: self.pending.as_ref().map(|p| PendingStatus {
                service: p.service,
                observed_at_ms: p.evidence.observed_at_ms,
                response_ready: p.task.is_finished(),
            }),
        }
    }
    pub(super) async fn take_ready(&mut self) -> Option<CompletedInference> {
        if !self.pending.as_ref().is_some_and(|p| p.task.is_finished()) {
            return None;
        }
        let mut pending = self.pending.take().unwrap();
        let (evidence, result) = match (&mut pending.task).await {
            Ok(r) => r,
            Err(_) => (
                pending.evidence.clone(),
                Inference::failed("worker_failed", "Jev evaluation task did not complete"),
            ),
        };
        if self.datadog() {
            if let Some(t) = &evidence.telemetry {
                self.latest_evidence_at = Some(t.short_window.end_unix_ms);
            }
            self.telemetry_status = result
                .error
                .as_ref()
                .filter(|e| e.code == "datadog_evidence")
                .map_or_else(|| "Datadog evidence received".into(), |e| e.message.clone());
        }
        Some(CompletedInference {
            id: pending.id,
            service: pending.service,
            evidence,
            inference: result,
            trace: pending
                .trace
                .take()
                .expect("pending decision owns its trace"),
        })
    }
    pub fn dispatch(&mut self, simulation: &LiveSimulation) -> Result<(), Error> {
        if self.pending.is_some()
            || simulation.policy() != PolicyKind::Jev
            || self.calls.load(Ordering::Relaxed) >= self.settings.max_evaluations
        {
            return Ok(());
        }
        let interval = if self.datadog() {
            Duration::from_secs(10)
        } else {
            self.settings.dispatch_interval
        };
        if self.last_dispatch.is_some_and(|at| at.elapsed() < interval) {
            return Ok(());
        }
        let Some(evaluator) = &self.evaluator else {
            return Err(Error::Invalid(
                "Jev requires TYPESAFE_API_KEY on the server".into(),
            ));
        };
        let evidence = simulation.evidence();
        for offset in 0..3 {
            let service = (self.round_robin + offset) % 3;
            let Some((_, state)) = evidence.iter().find(|(i, e)| {
                *i == service && e.legal_actions.len() > 1 && simulation.now() >= self.next_due[*i]
            }) else {
                continue;
            };
            let evaluator = evaluator.clone();
            let mut state = state.clone();
            state.forecast = self.forecasts[service].evidence(simulation.now() as u64);
            let source = evaluator.evidence_source();
            if source.is_some() {
                state.last_1_second = Default::default();
                state.last_5_seconds = Default::default();
            }
            let mut captured = state.clone();
            if source.is_none() {
                self.calls.fetch_add(1, Ordering::Relaxed);
            }
            self.attempts += 1;
            let calls = self.calls.clone();
            let run = self.run.clone();
            let upstream = simulation.service_id(service).to_owned();
            let floor = self
                .active_since
                .max(if state.phase == crate::policy::CircuitPhase::Closed {
                    state.control_since_unix_ms
                } else {
                    0
                })
                .saturating_add(crate::datadog::TRANSITION_MARGIN_MS);
            // HTTP awaits outside the session lock. Simulation and fault controls continue.
            let cost = self.cost.clone();
            if source.is_none() {
                cost.lock().unwrap().dispatched();
            }
            let trace = DecisionTrace::new(
                simulation.service_id(service),
                self.attempts,
                crate::telemetry::phase(state.phase),
                simulation.now(),
            );
            let span = trace.child("evaluate");
            let task = tokio::spawn(
                trace.scope(
                    async move {
                        if let Some(source) = source {
                            let result = tokio::time::timeout(
                                Duration::from_secs(7),
                                source.fetch(&upstream, &run, floor),
                            )
                            .await;
                            match result {
                                Ok(Ok(data)) => captured.telemetry = Some(data),
                                other => {
                                    let message = match other {
                                        Ok(Err(message)) => message,
                                        _ => "Datadog evidence query exceeded its deadline".into(),
                                    };
                                    return (
                                        captured,
                                        Inference::failed("datadog_evidence", message),
                                    );
                                }
                            }
                            if captured.phase == crate::policy::CircuitPhase::Closed
                                && captured.telemetry.as_ref().unwrap().short_window.responses < 10.
                            {
                                captured
                                    .legal_actions
                                    .retain(|a| *a != crate::jev::Choice::Open);
                            }
                            cost.lock().unwrap().dispatched();
                            calls.fetch_add(1, Ordering::Relaxed);
                        }
                        let result = evaluator.evaluate(captured.clone()).await;
                        // Meter on response arrival, including while simulation is paused.
                        // Guard rejection or replay must not change money already spent.
                        cost.lock().unwrap().record(&result);
                        (captured, result)
                    }
                    .instrument(span),
                ),
            );
            self.pending = Some(Pending {
                id: self.attempts,
                service,
                evidence: state,
                task,
                trace: Some(trace),
            });
            self.next_due[service] = simulation.now() + 3000.0;
            self.round_robin = (service + 1) % 3;
            self.last_dispatch = Some(Instant::now());
            break;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unknown_rates_and_missing_usage_are_excluded_not_assumed_free() {
        let mut cost = CostStatus::default();
        cost.dispatched();
        cost.record(&Inference::failed("timeout", "deadline"));
        cost.dispatched();
        let mut result = Inference::failed("unused", "fixture");
        result.model = Some("future-model".into());
        result.usage = Some(typesafe_ai::Usage {
            input_tokens: 700,
            output_tokens: 25,
            extra: Default::default(),
        });
        cost.record(&result);
        assert_eq!(cost.calls, 2);
        assert_eq!(cost.missing_usage_calls, 1);
        assert_eq!(cost.unpriced_calls, 1);
        assert_eq!(cost.priced_calls, 0);
        assert_eq!(cost.input_tokens, 700);
        assert_eq!(cost.estimated_usd, 0.0);
    }
}

#[cfg(test)]
mod datadog_driver_tests {
    use super::*;
    use crate::{
        datadog::Source,
        jev::{Choice, EvaluationFuture},
        playground::{Command, Session},
    };
    use axum::{routing::post, Json, Router};
    use serde_json::{json, Value};
    struct Judge {
        source: Arc<Source>,
        input: Arc<Mutex<Option<Value>>>,
    }
    impl Evaluator for Judge {
        fn evidence_source(&self) -> Option<Arc<Source>> {
            Some(self.source.clone())
        }
        fn evaluate(&self, e: Evidence) -> EvaluationFuture<'_> {
            *self.input.lock().unwrap() = Some(e.model_input());
            Box::pin(async {
                let mut r = Inference::failed("unused", "fixture");
                r.error = None;
                r.action = Some(Choice::NoChange);
                r
            })
        }
    }
    #[tokio::test]
    async fn background_datadog_fetch_replaces_local_health_and_does_not_block_traffic() {
        let arrived = Arc::new(tokio::sync::Notify::new());
        let a = arrived.clone();
        let resume = Arc::new(tokio::sync::Notify::new());
        let r = resume.clone();
        let app=Router::new().route("/api/v2/query/timeseries",post(move |Json(body):Json<Value>| {let a=a.clone();let r=r.clone();async move {
            a.notify_one(); r.notified().await;
            let to=body["data"]["attributes"]["to"].as_u64().unwrap();
            let times:Vec<_>=(0..13).map(|i|to-120_000+i*10_000).collect();
            let values:Vec<_>=[100.,80.,10.,5.,5.,24.,8.,1.,3.,9.].into_iter().map(|v|vec![Some(v);13]).collect();
            Json(json!({"data":{"attributes":{"times":times,"series":(0..10).map(|i|json!({"query_index":i})).collect::<Vec<_>>(),"values":values}}}))
        }})).route("/api/v2/query/scalar",post(||async {axum::http::StatusCode::BAD_REQUEST}));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let input = Arc::new(Mutex::new(None));
        let evaluator = Arc::new(Judge {
            source: Arc::new(Source::for_test(&base)),
            input: input.clone(),
        });
        let mut driver = Driver::new(Some(evaluator), JevSettings::default());
        driver.active_since = crate::datadog::unix_ms() - 180_000;
        let mut simulation = LiveSimulation::with_policy(42, PolicyKind::Jev).unwrap();
        simulation.advance_to(1000.).await.unwrap();
        driver.dispatch(&simulation).unwrap();
        tokio::time::timeout(Duration::from_secs(2), arrived.notified())
            .await
            .unwrap();
        assert_eq!(driver.status().calls, 0);
        assert!(input.lock().unwrap().is_none());
        tokio::time::timeout(Duration::from_secs(1), simulation.advance_to(1500.))
            .await
            .unwrap()
            .unwrap();
        resume.notify_one();
        let completed = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if let Some(v) = driver.take_ready().await {
                    break v;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(completed.inference.error.is_none());
        assert_eq!(driver.status().calls, 1);
        assert_eq!(driver.status().cost.calls, 1);
        assert_eq!(completed.evidence.last_5_seconds.responses, 0);
        let input = input.lock().unwrap().clone().unwrap();
        assert_eq!(input["telemetry"]["source"], "datadog");
        assert_eq!(input["telemetry"]["short_window"]["responses"], 285.);
        assert!(input.get("last_5_seconds").is_none());
        assert_eq!(
            input["telemetry"]["short_window"]["p95_latency_ms"],
            Value::Null
        );
        task.abort();
    }
    #[tokio::test]
    async fn warmup_never_calls_jev_and_real_time_controls_isolate_resets() {
        let input = Arc::new(Mutex::new(None));
        let evaluator = Arc::new(Judge {
            source: Arc::new(Source::for_test("http://127.0.0.1:1")),
            input: input.clone(),
        });
        let mut s =
            Session::configured(42, PolicyKind::Jev, Some(evaluator), JevSettings::default())
                .unwrap();
        let run = s.view().inference.simulation_run;
        assert_eq!(s.view().simulation.horizon_ms, 600_000.);
        for c in [
            Command::Step,
            Command::Replay,
            Command::Speed { value: 4 },
            Command::Policy {
                policy: PolicyKind::Threshold,
            },
        ] {
            assert!(s.command(c).await.is_err());
        }
        s.command(Command::Play).await.unwrap();
        s.tick(1000.).await.unwrap();
        assert!(
            s.view().simulation.at_ms < 1000.,
            "Tick arguments cannot accelerate wall-clock playback"
        );
        tokio::time::timeout(Duration::from_secs(2), async {
            while s.view().decisions.is_empty() {
                s.tick(50.).await.unwrap();
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(s.view().inference.calls, 0);
        assert_eq!(s.view().inference.cost.calls, 0);
        assert!(input.lock().unwrap().is_none());
        assert_eq!(
            s.view().decisions[0].inference.error.as_ref().unwrap().code,
            "datadog_evidence"
        );
        s.command(Command::Reset).await.unwrap();
        assert_ne!(s.view().inference.simulation_run, run);
    }
}
