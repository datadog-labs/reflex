// Unless explicitly stated otherwise all files in this repository are licensed under the Apache-2.0 License.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-present Datadog, Inc.

//! Loopback-only HTTP host for the interactive simulation. No external assets.
use crate::{
    engine::live::{Faults, Injection, LiveSimulation, LiveView},
    Error,
};
use axum::{
    extract::{DefaultBodyLimit, State},
    http::{header, Request, StatusCode},
    middleware::{self, Next},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::{collections::VecDeque, sync::Arc};
use tokio::sync::Mutex;

pub mod inference;
use crate::jev::{DecisionRecord, Evaluator, PolicyKind};
use inference::{Driver, InferenceStatus, JevSettings};
use tracing::Instrument;

#[derive(Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum TimelineEvent {
    Fault(Injection),
    Decision(Box<DecisionRecord>),
}
impl TimelineEvent {
    fn at(&self) -> f64 {
        match self {
            Self::Fault(i) => i.at_ms,
            Self::Decision(d) => d.completed_at_ms,
        }
    }
}

type Shared = Arc<Mutex<Session>>;
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IncidentScenario {
    #[default]
    Sandbox,
    SlowdownSurge,
    ErrorWaves,
}
impl IncidentScenario {
    fn events(self, remote: bool) -> Vec<(f64, usize, Faults)> {
        let scale = if remote { 3. } else { 1. };
        let rows = match self {
            Self::Sandbox => vec![],
            Self::SlowdownSurge => vec![
                (
                    75.,
                    1,
                    Faults {
                        slow: true,
                        surge: true,
                        errors: false,
                    },
                ),
                (120., 1, Faults::default()),
            ],
            Self::ErrorWaves => vec![
                (
                    75.,
                    2,
                    Faults {
                        errors: true,
                        ..Default::default()
                    },
                ),
                (100., 2, Faults::default()),
                (
                    125.,
                    2,
                    Faults {
                        errors: true,
                        ..Default::default()
                    },
                ),
                (150., 2, Faults::default()),
            ],
        };
        rows.into_iter()
            .map(|(t, i, f)| (t * 1000. * scale, i, f))
            .collect()
    }
    fn description(self, remote: bool) -> String {
        let k = if remote { 3 } else { 1 };
        match self {
            Self::Sandbox=>"Inject your own faults. No scheduled changes.".into(),
            Self::SlowdownSurge=>format!("Payments slows 6× and receives 4× traffic at {}s; both recover at {}s. Watch pressure build, the circuit open, and recovery probes.",75*k,120*k),
            Self::ErrorWaves=>format!("Search gets an 85% error storm at {}–{}s, then {}–{}s. Watch opening, probing, and recovery between waves.",75*k,100*k,125*k,150*k),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Command {
    Scenario { scenario: IncidentScenario },
    Forecast { enabled: bool },
    Policy { policy: PolicyKind },
    Play,
    Pause,
    Step,
    Speed { value: u8 },
    Fault { service: usize, faults: Faults },
    Repair,
    Reset,
    Replay,
}
#[derive(Serialize)]
pub struct SessionView {
    pub scenario: IncidentScenario,
    pub scenario_description: String,
    pub forecasts: Vec<crate::forecasting::View>,
    #[serde(flatten)]
    pub simulation: LiveView,
    pub paused: bool,
    pub speed: u8,
    pub replaying: bool,
    pub error: Option<String>,
    pub policy: PolicyKind,
    pub inference: InferenceStatus,
    pub decisions: Vec<DecisionRecord>,
}
pub struct Session {
    scenario: IncidentScenario,
    script_cursor: usize,
    simulation: LiveSimulation,
    paused: bool,
    speed: u8,
    replay: Option<(VecDeque<TimelineEvent>, f64)>,
    error: Option<String>,
    driver: Driver,
    timeline: Vec<TimelineEvent>,
    decisions: Vec<DecisionRecord>,
    wall_tick: Option<std::time::Instant>,
}
impl Session {
    pub fn new(seed: u64) -> Result<Self, Error> {
        Self::configured(seed, PolicyKind::Threshold, None, JevSettings::default())
    }
    pub fn configured(
        seed: u64,
        policy: PolicyKind,
        evaluator: Option<Arc<dyn Evaluator>>,
        settings: JevSettings,
    ) -> Result<Self, Error> {
        if policy == PolicyKind::Jev && evaluator.is_none() {
            return Err(Error::Invalid(
                "set TYPESAFE_API_KEY before selecting Jev".into(),
            ));
        }
        if settings.max_evaluations == 0
            || settings.max_evaluations > 1000
            || settings.model.trim().is_empty()
        {
            return Err(Error::Invalid(
                "Jev requires a model and a call limit between 1 and 1000".into(),
            ));
        }
        let driver = Driver::new(evaluator, settings);
        if driver.datadog() && policy != PolicyKind::Jev {
            return Err(Error::Invalid(
                "Datadog evidence mode requires the Jev policy".into(),
            ));
        }
        let simulation = if driver.datadog() {
            LiveSimulation::with_datadog(seed, driver.run())?
        } else {
            LiveSimulation::with_policy(seed, policy)?
        };
        Ok(Self {
            scenario: IncidentScenario::Sandbox,
            script_cursor: 0,
            simulation,
            paused: true,
            speed: 1,
            replay: None,
            error: None,
            driver,
            wall_tick: None,
            timeline: vec![],
            decisions: vec![],
        })
    }
    pub fn view(&self) -> SessionView {
        SessionView {
            scenario: self.scenario,
            scenario_description: self.scenario.description(self.driver.datadog()),
            forecasts: self
                .driver
                .forecasts
                .iter()
                .map(|f| f.view(self.simulation.now() as u64))
                .collect(),
            simulation: self.simulation.view(),
            paused: self.paused,
            speed: self.speed,
            replaying: self.replay.is_some(),
            error: self.error.clone(),
            policy: self.simulation.policy(),
            inference: self.driver.status(),
            decisions: self.decisions.iter().rev().take(30).cloned().collect(),
        }
    }
    pub async fn command(&mut self, command: Command) -> Result<(), Error> {
        if self.driver.datadog()
            && matches!(
                command,
                Command::Step
                    | Command::Replay
                    | Command::Speed { value: 2 | 4 }
                    | Command::Policy {
                        policy: PolicyKind::Threshold
                    }
            )
        {
            return Err(Error::Invalid("Datadog evidence uses live Jev at 1× speed; stepping, accelerated playback, replay and threshold mode are unavailable".into()));
        }
        let operation = match &command {
            Command::Play => Some("play"),
            Command::Pause => Some("pause"),
            Command::Reset => Some("reset"),
            Command::Repair => Some("repair"),
            Command::Replay => Some("replay"),
            Command::Policy { .. } => Some("policy"),
            _ => None,
        };
        match command {
            Command::Scenario { scenario } => {
                self.scenario = scenario;
                self.reset_with_policy(self.simulation.policy())?;
            }
            Command::Forecast { enabled } => {
                if self.replay.is_some() {
                    return Err(Error::Invalid(
                        "Finish replay before changing forecasting".into(),
                    ));
                }
                if enabled && self.driver.forecasts.iter().any(|f| f.provider.is_none()) {
                    return Err(Error::Invalid(
                        "Toto is not configured on this server".into(),
                    ));
                }
                self.driver.cancel();
                for f in &mut self.driver.forecasts {
                    f.set_enabled(enabled);
                }
            }
            Command::Policy { policy } => {
                self.reset_with_policy(policy)?;
            }
            Command::Play => {
                if self.error.is_some() {
                    return Err(Error::Invalid(
                        "reset the incident after an engine error".into(),
                    ));
                }
                if self.simulation.now() >= self.simulation.horizon() {
                    return Err(Error::Invalid(
                        "session complete; reset or replay the incident".into(),
                    ));
                }
                if self.paused && self.driver.datadog() {
                    self.driver.resume();
                }
                self.wall_tick = Some(std::time::Instant::now());
                self.paused = false;
            }
            Command::Pause => {
                self.paused = true;
                if self.driver.datadog() {
                    self.driver.cancel();
                }
            }
            Command::Step => {
                self.paused = true;
                self.advance(1000.0).await?;
            }
            Command::Speed { value } => {
                if ![1, 2, 4].contains(&value) {
                    return Err(Error::Invalid("speed must be 1, 2, or 4".into()));
                }
                self.speed = value;
            }
            Command::Fault { service, faults } => {
                if self.replay.is_some() {
                    return Err(Error::Invalid(
                        "finish the replay before editing faults".into(),
                    ));
                }
                if self.simulation.now() >= self.simulation.horizon() {
                    return Err(Error::Invalid(
                        "reset before editing a completed incident".into(),
                    ));
                }
                let before = self.simulation.injections().len();
                self.simulation.set_faults(service, faults)?;
                self.record_faults(before);
            }
            Command::Repair => {
                if self.replay.is_some() {
                    return Err(Error::Invalid(
                        "finish the replay before editing faults".into(),
                    ));
                }
                let before = self.simulation.injections().len();
                self.simulation.repair_all()?;
                self.record_faults(before);
            }
            Command::Reset => {
                self.reset_with_policy(self.simulation.policy())?;
            }
            Command::Replay => {
                let end = self.simulation.now();
                if end == 0.0 {
                    return Err(Error::Invalid(
                        "run the simulation before replaying an incident".into(),
                    ));
                }
                let edits = self.timeline.iter().cloned().collect();
                self.driver.cancel();
                self.simulation =
                    LiveSimulation::with_policy(self.simulation.seed(), self.simulation.policy())?
                        .without_telemetry();
                self.timeline.clear();
                self.decisions.clear();
                self.replay = Some((edits, end));
                self.error = None;
                self.paused = false;
            }
        }
        if let Some(operation) = operation {
            tracing::info!(target: "reflex_sim::incident", operation,
                simulation_time_ms = self.simulation.now(), "Circuit breaker playground control applied");
        }
        Ok(())
    }
    fn reset_with_policy(&mut self, policy: PolicyKind) -> Result<(), Error> {
        let mut next = Self::configured(
            self.simulation.seed(),
            policy,
            self.driver.evaluator.clone(),
            self.driver.settings.clone(),
        )?;
        next.scenario = self.scenario;
        next.driver.cost = self.driver.cost.clone();
        for i in 0..3 {
            next.driver.forecasts[i].provider = self.driver.forecasts[i].provider.clone();
            next.driver.forecasts[i].enabled = self.driver.forecasts[i].enabled;
        }
        *self = next;
        Ok(())
    }
    fn record_faults(&mut self, before: usize) {
        self.timeline.extend(
            self.simulation.injections()[before..]
                .iter()
                .cloned()
                .map(TimelineEvent::Fault),
        );
    }
    async fn service_inference(&mut self) -> Result<(), Error> {
        if self.replay.is_none() && self.simulation.now() < self.simulation.horizon() {
            self.driver.forecast(self.simulation.view()).await;
        }
        if self.simulation.policy() != PolicyKind::Jev
            || self.replay.is_some()
            || self.simulation.now() >= self.simulation.horizon()
        {
            return Ok(());
        }
        if let Some(inference::CompletedInference {
            id,
            service,
            evidence,
            inference,
            mut trace,
        }) = self.driver.take_ready().await
        {
            let span = trace.child("apply");
            let result = trace
                .scope(
                    self.simulation
                        .apply_model(service, evidence.clone(), inference.clone())
                        .instrument(span),
                )
                .await;
            trace.finish(
                result
                    .as_ref()
                    .map_or("error", |guard| guard.status.as_str()),
            );
            let guard = result?;
            let record = DecisionRecord {
                id,
                service,
                completed_at_ms: self.simulation.now(),
                model_input: evidence.model_input(),
                evidence,
                inference,
                guard,
            };
            self.timeline
                .push(TimelineEvent::Decision(Box::new(record.clone())));
            self.decisions.push(record);
        }
        self.driver.dispatch(&self.simulation)
    }
    async fn advance(&mut self, delta: f64) -> Result<(), Error> {
        let end = self
            .replay
            .as_ref()
            .map_or(self.simulation.horizon(), |r| r.1);
        let target = (self.simulation.now() + delta).min(end);
        if self.replay.is_none() {
            let events = self.scenario.events(self.driver.datadog());
            while let Some((at, service, faults)) = events
                .get(self.script_cursor)
                .copied()
                .filter(|(at, _, _)| *at <= target)
            {
                self.simulation.advance_to(at).await?;
                let before = self.simulation.injections().len();
                self.simulation.set_faults(service, faults)?;
                self.record_faults(before);
                self.script_cursor += 1;
            }
        }
        loop {
            let next = self
                .replay
                .as_ref()
                .and_then(|r| r.0.front())
                .filter(|i| i.at() <= target)
                .cloned();
            let Some(injection) = next else {
                break;
            };
            self.simulation.advance_to(injection.at()).await?;
            match &injection {
                TimelineEvent::Fault(i) => self.simulation.set_faults(i.service, i.faults)?,
                TimelineEvent::Decision(d) => {
                    let guard = self
                        .simulation
                        .apply_model(d.service, d.evidence.clone(), d.inference.clone())
                        .await?;
                    if guard.status != d.guard.status || guard.to != d.guard.to {
                        return Err(Error::Policy(
                            "recorded decision did not reproduce its guard result".into(),
                        ));
                    }
                    self.decisions.push((**d).clone());
                }
            }
            self.timeline.push(injection);
            self.replay.as_mut().unwrap().0.pop_front();
        }
        self.simulation.advance_to(target).await?;
        if self.replay.is_none() {
            self.service_inference().await?;
        }
        if target >= end {
            self.paused = true;
            self.replay = None;
        }
        Ok(())
    }
    pub async fn tick(&mut self, elapsed_ms: f64) -> Result<(), Error> {
        if !elapsed_ms.is_finite() || !(0.0..=1000.0).contains(&elapsed_ms) {
            return Err(Error::Invalid("invalid clock tick".into()));
        }
        if !self.paused {
            let elapsed_ms = if self.driver.datadog() {
                let now = std::time::Instant::now();
                let delta = self
                    .wall_tick
                    .replace(now)
                    .map_or(0., |last| now.duration_since(last).as_secs_f64() * 1000.);
                delta.min(1000.)
            } else {
                elapsed_ms
            };
            self.advance(elapsed_ms * f64::from(self.speed)).await?;
        }
        Ok(())
    }
}
async fn state(State(shared): State<Shared>) -> Json<SessionView> {
    Json(shared.lock().await.view())
}
async fn command(State(shared): State<Shared>, Json(command): Json<Command>) -> Response {
    let mut session = shared.lock().await;
    match session.command(command).await {
        Ok(()) => Json(session.view()).into_response(),
        Err(error) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error":error.to_string()})),
        )
            .into_response(),
    }
}
async fn export(State(shared): State<Shared>) -> Response {
    let session = shared.lock().await;
    match serde_json::to_string(
        &serde_json::json!({"incident":session.simulation.export(),"decisions":session.decisions,"timeline":session.timeline,"policy":session.simulation.policy(),"inference":session.driver.status()}),
    ) {
        Ok(data) => (
            [
                (header::CONTENT_TYPE, "application/json"),
                (
                    header::CONTENT_DISPOSITION,
                    "attachment; filename=reflex-incident.json",
                ),
            ],
            data,
        )
            .into_response(),
        Err(error) => (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()).into_response(),
    }
}
async fn local_only(request: Request<axum::body::Body>, next: Next) -> Response {
    let host = request
        .headers()
        .get(header::HOST)
        .and_then(|s| s.to_str().ok())
        .unwrap_or("");
    let local = host.rsplit_once(':').is_some_and(|(name, port)| {
        matches!(name, "127.0.0.1" | "localhost") && port.parse::<u16>().is_ok()
    });
    let origin_ok = request
        .headers()
        .get(header::ORIGIN)
        .is_none_or(|o| o.to_str().is_ok_and(|o| o == format!("http://{host}")));
    if !local || !origin_ok {
        return StatusCode::FORBIDDEN.into_response();
    }
    let mut response = next.run(request).await;
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    response
        .headers_mut()
        .insert(header::X_CONTENT_TYPE_OPTIONS, "nosniff".parse().unwrap());
    response
}
fn router(shared: Shared) -> Router {
    Router::new()
        .route(
            "/",
            get(|| async { Html(include_str!("playground/index.html")) }),
        )
        .route(
            "/app.css",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/css")],
                    include_str!("playground/app.css"),
                )
            }),
        )
        .route(
            "/app.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript")],
                    include_str!("playground/app.js"),
                )
            }),
        )
        .route(
            "/controls.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript")],
                    include_str!("playground/controls.js"),
                )
            }),
        )
        .route(
            "/controls.css",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/css")],
                    include_str!("playground/controls.css"),
                )
            }),
        )
        .route(
            "/typography.css",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/css")],
                    include_str!("typography.css"),
                )
            }),
        )
        .route("/scenario-ui.js", get(|| async { ([(header::CONTENT_TYPE, "text/javascript")], include_str!("playground/scenario-ui.js")) }))
        .route("/scenario-ui.css", get(|| async { ([(header::CONTENT_TYPE, "text/css")], include_str!("playground/scenario-ui.css")) }))
        .route("/scenario.css", get(|| async { ([(header::CONTENT_TYPE, "text/css")], include_str!("playground/scenario.css")) }))
        .route(
            "/forecast.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript")],
                    include_str!("playground/forecast.js"),
                )
            }),
        )
        .route(
            "/forecast.css",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/css")],
                    include_str!("playground/forecast.css"),
                )
            }),
        )
        .route("/api/state", get(state))
        .route("/api/command", post(command))
        .route("/api/export", get(export))
        .layer(DefaultBodyLimit::max(16 * 1024))
        .layer(middleware::from_fn(local_only))
        .with_state(shared)
}
pub async fn serve(seed: u64, port: u16, open: bool) -> Result<(), Error> {
    serve_configured(
        seed,
        port,
        open,
        PolicyKind::Threshold,
        None,
        JevSettings::default(),
    )
    .await
}
pub async fn serve_configured(
    seed: u64,
    port: u16,
    open: bool,
    policy: PolicyKind,
    evaluator: Option<Arc<dyn Evaluator>>,
    settings: JevSettings,
) -> Result<(), Error> {
    serve_with_scheduler(seed, port, open, policy, evaluator, settings, None).await
}
pub async fn serve_with_scheduler(
    seed: u64,
    port: u16,
    open: bool,
    policy: PolicyKind,
    evaluator: Option<Arc<dyn Evaluator>>,
    settings: JevSettings,
    scheduler_evaluator: Option<Arc<dyn crate::scheduler::judge::Evaluator>>,
) -> Result<(), Error> {
    serve_with_recovery(
        seed,
        port,
        open,
        policy,
        evaluator,
        settings,
        scheduler_evaluator,
        None,
    )
    .await
}
#[allow(clippy::too_many_arguments)]
pub async fn serve_with_recovery(
    seed: u64,
    port: u16,
    open: bool,
    policy: PolicyKind,
    evaluator: Option<Arc<dyn Evaluator>>,
    settings: JevSettings,
    scheduler_evaluator: Option<Arc<dyn crate::scheduler::judge::Evaluator>>,
    recovery_evaluator: Option<Arc<dyn crate::recovery::judge::Evaluator>>,
) -> Result<(), Error> {
    serve_with_forecasts(
        seed,
        port,
        open,
        policy,
        evaluator,
        settings,
        scheduler_evaluator,
        recovery_evaluator,
        None,
    )
    .await
}
#[allow(clippy::too_many_arguments)]
pub async fn serve_with_forecasts(
    seed: u64,
    port: u16,
    open: bool,
    policy: PolicyKind,
    evaluator: Option<Arc<dyn Evaluator>>,
    settings: JevSettings,
    scheduler_evaluator: Option<Arc<dyn crate::scheduler::judge::Evaluator>>,
    recovery_evaluator: Option<Arc<dyn crate::recovery::judge::Evaluator>>,
    forecaster: Option<Arc<dyn crate::capacity::forecast::Forecaster>>,
) -> Result<(), Error> {
    let recovery = Arc::new(Mutex::new(crate::recovery::Session::new(
        seed,
        recovery_evaluator,
        settings.clone(),
    )?));
    recovery.lock().await.forecast.provider = forecaster.clone();
    let recovery_clock = recovery.clone();
    let recovery_task = tokio::spawn(async move {
        let mut previous = std::time::Instant::now();
        let mut interval = tokio::time::interval(std::time::Duration::from_millis(50));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            let mut session = recovery_clock.lock().await;
            let elapsed = previous.elapsed().as_millis() as u64;
            previous += std::time::Duration::from_millis(elapsed);
            let ms = if session.uses_datadog() { elapsed } else { 50 };
            if let Err(e) = session.tick(ms).await {
                let _ = session.command(crate::recovery::Command::Pause).await;
                session.error = Some(e.to_string());
            }
        }
    });
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port)).await?;
    let scheduler = Arc::new(Mutex::new(crate::scheduler::Session::new(
        seed,
        scheduler_evaluator,
        settings.clone(),
    )?));
    scheduler.lock().await.forecast.provider = forecaster.clone();
    let scheduler_clock = scheduler.clone();
    let scheduler_task = tokio::spawn(async move {
        let mut previous = std::time::Instant::now();
        let mut interval = tokio::time::interval(std::time::Duration::from_millis(50));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            let mut session = scheduler_clock.lock().await;
            let now = std::time::Instant::now();
            let elapsed = now.duration_since(previous).as_millis() as u64;
            previous += std::time::Duration::from_millis(elapsed);
            let ms = if session.uses_datadog() { elapsed } else { 50 };
            if let Err(e) = session.tick(ms).await {
                let _ = session.command(crate::scheduler::Command::Pause).await;
                session.error = Some(e.to_string());
            }
        }
    });
    let url = format!("http://{}/", listener.local_addr()?);
    let shared = Arc::new(Mutex::new(Session::configured(
        seed, policy, evaluator, settings,
    )?));
    for f in &mut shared.lock().await.driver.forecasts {
        f.provider = forecaster.clone();
    }
    let clock = shared.clone();
    let task = tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_millis(50));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            let mut session = clock.lock().await;
            if let Err(error) = session.tick(50.0).await {
                session.paused = true;
                session.error = Some(error.to_string());
            }
        }
    });
    println!("Incident Playground: {url}\nSeed: {seed} · policy: {policy:?}\nPress Ctrl+C to stop. Browser tabs share this session.");
    if open {
        if let Err(error) = crate::report::open_target(&url) {
            eprintln!("Could not launch browser: {error}. Open {url} manually.");
        }
    }
    let app = router(shared)
        .merge(crate::scheduler::web::router(scheduler))
        .merge(crate::recovery::web::router(recovery))
        .layer(DefaultBodyLimit::max(16 * 1024))
        .layer(middleware::from_fn(local_only));
    let result = axum::serve(listener, app).await;

    recovery_task.abort();
    scheduler_task.abort();
    task.abort();
    result.map_err(Error::Io)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn http_controls_use_rust_state_and_reject_external_origins() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let app = router(Arc::new(Mutex::new(Session::new(42).unwrap())));
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let client = reqwest::Client::new();
        let initial: serde_json::Value = client
            .get(format!("{url}/api/state"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(initial["paused"], true);
        let response = client
            .post(format!("{url}/api/command"))
            .json(&serde_json::json!({"type":"step"}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let stepped: serde_json::Value = response.json().await.unwrap();
        assert_eq!(stepped["at_ms"], 1000.0);
        assert!(stepped["counts"]["offered"].as_u64().unwrap() > 0);
        let external = client
            .post(format!("{url}/api/command"))
            .header("Origin", "https://example.com")
            .json(&serde_json::json!({"type":"reset"}))
            .send()
            .await
            .unwrap();
        assert_eq!(external.status(), StatusCode::FORBIDDEN);
        let bad_host = client
            .get(format!("{url}/api/state"))
            .header("Host", "example.com:8742")
            .send()
            .await
            .unwrap();
        assert_eq!(bad_host.status(), StatusCode::FORBIDDEN);
        let invalid=client.post(format!("{url}/api/command")).json(&serde_json::json!({"type":"fault","service":99,"faults":{"slow":true,"errors":false,"surge":false}})).send().await.unwrap();
        assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);
        let export = client
            .get(format!("{url}/api/export"))
            .send()
            .await
            .unwrap();
        assert_eq!(
            export.headers()[header::CONTENT_DISPOSITION],
            "attachment; filename=reflex-incident.json"
        );
        let exported: serde_json::Value = export.json().await.unwrap();
        assert_eq!(exported["incident"]["at_ms"], 1000.0);
        assert_eq!(exported["incident"]["algorithm"], "reflex-threshold-v1");
        assert!(!exported["incident"]["trace"]["requests"]
            .as_array()
            .unwrap()
            .is_empty());
        task.abort();
    }
}

#[cfg(test)]
mod forecast_tests {
    use super::*;
    use std::sync::Mutex as StdMutex;
    struct Capture(Arc<StdMutex<Vec<serde_json::Value>>>);
    impl crate::jev::Evaluator for Capture {
        fn evaluate(&self, e: crate::jev::Evidence) -> crate::jev::EvaluationFuture<'_> {
            self.0.lock().unwrap().push(e.model_input());
            Box::pin(async { crate::jev::Inference::failed("fixture", "test abstention") })
        }
    }
    #[tokio::test]
    async fn all_three_service_forecasts_reach_jev_and_reset_clears_them() {
        let input = Arc::new(StdMutex::new(vec![]));
        let settings = JevSettings {
            dispatch_interval: std::time::Duration::ZERO,
            max_evaluations: 1000,
            ..Default::default()
        };
        let mut session = Session::configured(
            42,
            PolicyKind::Jev,
            Some(Arc::new(Capture(input.clone()))),
            settings,
        )
        .unwrap();
        let mock = Arc::new(crate::forecasting::tests::Recording(StdMutex::new(vec![])));
        for f in &mut session.driver.forecasts {
            f.provider = Some(mock.clone());
        }
        for _ in 0..80 {
            session.command(Command::Step).await.unwrap();
            tokio::task::yield_now().await;
        }
        assert!(session
            .view()
            .forecasts
            .iter()
            .all(|f| f.forecast.is_some()));
        for service in ["Catalog", "Payments", "Search"] {
            assert!(
                input.lock().unwrap().iter().any(|e| e["service"] == service
                    && e["forecast"]["source"] == "simulator_observations"),
                "{service}"
            );
        }
        session.command(Command::Reset).await.unwrap();
        assert!(session
            .view()
            .forecasts
            .iter()
            .all(|f| f.forecast.is_none()));
    }
}
