// Unless explicitly stated otherwise all files in this repository are licensed under the Apache-2.0 License.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-present Datadog, Inc.

//! Forecast → judgment → guarded capacity transitions. All policies share offered traffic.
pub mod engine;
pub mod forecast;
pub mod judge;
pub mod web;
pub mod workload;
use crate::{
    playground::inference::{CostStatus, JevSettings},
    Error,
};
use engine::{Action, Engine, Event, JobPhase, Lifecycle, Proposal, Settings, HORIZON_MS};
use forecast::{Forecaster, Input, ResultRecord, Snapshot};
use judge::{Evaluator, Evidence, Inference};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::task::JoinHandle;
use workload::{Bucket, Client, Scenario};
const LABELS: [&str; 3] = ["Reactive thresholds", "Toto + thresholds", "Toto + Jev"];
#[derive(Default)]
pub struct Options {
    pub recording: Option<Recording>,
    pub forecaster: Option<Arc<dyn Forecaster>>,
    pub evaluator: Option<Arc<dyn Evaluator>>,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Decision {
    pub at_ms: u64,
    pub lane: usize,
    pub source: String,
    pub evidence: Evidence,
    pub result: Inference,
    pub action: Action,
    pub applied: bool,
    pub reason: String,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Entry {
    Clients { at_ms: u64, clients: Vec<Client> },
    Tick { bucket: Bucket },
    Forecast { record: ResultRecord },
    Decision { decision: Box<Decision> },
    Configure { at_ms: u64, settings: Settings },
}
impl Entry {
    fn at(&self) -> u64 {
        match self {
            Self::Clients { at_ms, .. } => *at_ms,
            Self::Tick { bucket } => bucket.at_ms as u64,
            Self::Forecast { record } => record.available_at_ms,
            Self::Decision { decision } => decision.at_ms,
            Self::Configure { at_ms, .. } => *at_ms,
        }
    }
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Recording {
    pub version: u32,
    pub seed: u64,
    pub scenario: Scenario,
    pub clients: Vec<Client>,
    pub settings: Settings,
    pub prehistory: Vec<Bucket>,
    pub end_ms: u64,
    pub entries: Vec<Entry>,
}
impl Recording {
    pub fn validate(&self) -> Result<(), Error> {
        let invalid = || Error::Invalid("Invalid or incompatible capacity recording".into());
        let valid_clients = |clients: &[Client]| {
            clients.len() <= 8
                && !clients.is_empty()
                && clients.iter().all(Client::valid)
                && clients
                    .iter()
                    .map(|c| c.id)
                    .collect::<std::collections::HashSet<_>>()
                    .len()
                    == clients.len()
        };
        if self.version != 1
            || !self.settings.valid()
            || !valid_clients(&self.clients)
            || self.end_ms > HORIZON_MS
            || !self.end_ms.is_multiple_of(1000)
            || self.prehistory.len() != forecast::CONTEXT
            || self.entries.len() > 10_000
        {
            return Err(invalid());
        }
        let mut history = self.prehistory.clone();
        for (i, b) in history.iter().enumerate() {
            if b.at_ms != (i as i64 - 255) * 1000
                || b.values.iter().any(|v| !v.is_finite() || *v < 0.)
            {
                return Err(invalid());
            }
        }
        let mut now = 0;
        for e in &self.entries {
            if e.at() > self.end_ms {
                return Err(invalid());
            }
            match e {
                Entry::Tick { bucket } => {
                    if bucket.at_ms != now as i64 + 1000
                        || bucket.jobs.len() > 128
                        || bucket.values.iter().any(|v| !v.is_finite() || *v < 0.)
                        || bucket.jobs.iter().any(|j| {
                            j.client >= 8
                                || j.cpu == 0
                                || j.cpu > 16
                                || j.memory_gib == 0
                                || j.memory_gib > 32
                                || j.estimated_s == 0
                                || j.estimated_s > 30
                                || j.actual_s == 0
                                || j.actual_s > 36
                        })
                    {
                        return Err(invalid());
                    }
                    let values = bucket.jobs.iter().fold([0f32; 3], |mut sum, j| {
                        sum[0] += 1.;
                        sum[1] += (j.cpu as u64 * j.estimated_s) as f32;
                        sum[2] += (j.memory_gib as u64 * j.estimated_s) as f32;
                        sum
                    });
                    if values != bucket.values {
                        return Err(invalid());
                    }
                    now += 1000;
                    history.push(bucket.clone());
                }
                Entry::Forecast { record } => {
                    if e.at() != now
                        || record.input.origin_ms > now
                        || !record.input.origin_ms.is_multiple_of(1000)
                        || record.snapshot.is_some() == record.error.is_some()
                    {
                        return Err(invalid());
                    }
                    let rows = forecast::CONTEXT + (record.input.origin_ms / 1000) as usize;
                    if record.input != workload::input(&history[..rows], record.input.origin_ms) {
                        return Err(invalid());
                    }
                    if let Some(f) = &record.snapshot {
                        f.validate(&record.input).map_err(Error::Invalid)?;
                    }
                }
                Entry::Clients { clients, .. } => {
                    if e.at() != now || !valid_clients(clients) {
                        return Err(invalid());
                    }
                }
                Entry::Configure { settings, .. } => {
                    if e.at() != now || !settings.valid() {
                        return Err(invalid());
                    }
                }
                Entry::Decision { decision } => {
                    if e.at() != now
                        || decision.lane >= 3
                        || decision.evidence.observed_at_ms > now
                        || decision
                            .evidence
                            .forecast
                            .as_ref()
                            .is_some_and(|f| f.origin_ms > decision.evidence.observed_at_ms)
                    {
                        return Err(invalid());
                    }
                }
            }
        }
        if now != self.end_ms {
            return Err(invalid());
        }
        Ok(())
    }
}
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Command {
    Play,
    Pause,
    Step,
    Reset,
    Replay,
    Speed { value: u64 },
    Scenario { scenario: Scenario },
    Client { client: Client },
    AddClient,
    RemoveClient { id: usize },
    Settings { settings: Settings },
}
struct Pending<T> {
    task: JoinHandle<T>,
}
impl<T> Drop for Pending<T> {
    fn drop(&mut self) {
        self.task.abort();
    }
}
#[derive(Clone, Default, Serialize)]
struct Accuracy {
    samples: u64,
    cpu_absolute_error: f64,
    cpu_covered: u64,
}
pub struct Session {
    seed: u64,
    scenario: Scenario,
    clients: Vec<Client>,
    settings: Settings,
    engines: Vec<Engine>,
    history: Vec<Bucket>,
    trend: Vec<Value>,
    latest: Option<Snapshot>,
    forecast_error: Option<String>,
    forecast_calls: usize,
    forecast_latency: f64,
    forecast_successes: usize,
    accuracy: Accuracy,
    forecaster: Option<Arc<dyn Forecaster>>,
    evaluator: Option<Arc<dyn Evaluator>>,
    jev_settings: JevSettings,
    cost: Arc<Mutex<CostStatus>>,
    jev_calls: usize,
    pending_forecast: Option<Pending<(Input, Result<Snapshot, String>)>>,
    pending_judge: Option<Pending<(Evidence, Inference)>>,
    last_forecast_wall: Option<Instant>,
    last_judge_wall: Option<Instant>,
    last_forecast_sim: Option<u64>,
    last_judge_sim: Option<u64>,
    last_baseline_sim: Option<u64>,
    recording: Recording,
    replay: Option<Recording>,
    replay_cursor: usize,
    decisions: Vec<Decision>,
    pub paused: bool,
    speed: u64,
    remainder: u64,
    pub error: Option<String>,
}
impl Session {
    pub fn new(seed: u64, options: Options, jev_settings: JevSettings) -> Result<Self, Error> {
        let clients = workload::defaults();
        let settings = Settings::default();
        let scenario = Scenario::Cycles;
        let history = workload::prehistory(seed, scenario, &clients);
        let engines = (0..3)
            .map(|_| Engine::new(settings.clone()))
            .collect::<Result<Vec<_>, _>>()?;
        let recording = Recording {
            version: 1,
            seed,
            scenario,
            clients: clients.clone(),
            settings: settings.clone(),
            prehistory: history.clone(),
            end_ms: 0,
            entries: vec![],
        };
        let recording_to_load = options.recording;
        let mut session = Self {
            seed,
            scenario,
            clients,
            settings,
            engines,
            history,
            trend: vec![],
            latest: None,
            forecast_error: None,
            forecast_calls: 0,
            forecast_latency: 0.,
            forecast_successes: 0,
            accuracy: Accuracy::default(),
            forecaster: options.forecaster,
            evaluator: options.evaluator,
            jev_settings,
            cost: Arc::new(Mutex::new(CostStatus::default())),
            jev_calls: 0,
            pending_forecast: None,
            pending_judge: None,
            last_forecast_wall: None,
            last_judge_wall: None,
            last_forecast_sim: None,
            last_judge_sim: None,
            last_baseline_sim: None,
            recording,
            replay: None,
            replay_cursor: 0,
            decisions: vec![],
            paused: true,
            speed: 1,
            remainder: 0,
            error: None,
        };
        if let Some(tape) = recording_to_load {
            session.load_recording(tape)?;
        }
        Ok(session)
    }
    pub fn load_recording(&mut self, tape: Recording) -> Result<(), Error> {
        tape.validate()?;
        self.seed = tape.seed;
        self.scenario = tape.scenario;
        self.clients = tape.clients.clone();
        self.settings = tape.settings.clone();
        self.reset()?;
        self.history = tape.prehistory.clone();
        self.replay = Some(tape);
        Ok(())
    }
    pub fn at(&self) -> u64 {
        self.history.last().unwrap().at_ms as u64
    }
    fn reset(&mut self) -> Result<(), Error> {
        self.pending_forecast = None;
        self.pending_judge = None;
        self.engines = (0..3)
            .map(|_| Engine::new(self.settings.clone()))
            .collect::<Result<Vec<_>, _>>()?;
        self.history = workload::prehistory(self.seed, self.scenario, &self.clients);
        self.trend.clear();
        self.latest = None;
        self.forecast_error = None;
        self.forecast_calls = 0;
        self.forecast_latency = 0.;
        self.forecast_successes = 0;
        self.accuracy = Accuracy::default();
        self.jev_calls = 0;
        self.last_forecast_sim = None;
        self.last_judge_sim = None;
        self.last_baseline_sim = None;
        self.decisions.clear();
        self.replay = None;
        self.replay_cursor = 0;
        self.paused = true;
        self.remainder = 0;
        self.error = None;
        self.recording = Recording {
            version: 1,
            seed: self.seed,
            scenario: self.scenario,
            clients: self.clients.clone(),
            settings: self.settings.clone(),
            prehistory: self.history.clone(),
            end_ms: 0,
            entries: vec![],
        };
        Ok(())
    }
    pub fn export(&self) -> Value {
        json!({"recording":self.replay.as_ref().unwrap_or(&self.recording),"summary":self.view(),"notes":"Synthetic 256-second prehistory; forecast quantiles are pointwise, not calibrated guarantees. Recorded replay issues no network calls. Jev cost is server-session cumulative; Toto monetary cost unavailable."})
    }
    pub fn view(&self) -> Value {
        let now = self.at();
        let lanes:Vec<_>=self.engines.iter().enumerate().map(|(i,e)|{let d=e.data();let jobs:Vec<_>=d.jobs.iter().filter(|j|matches!(j.phase,JobPhase::Running|JobPhase::Queued)).collect();let completed=d.jobs.iter().filter(|j|j.phase==JobPhase::Completed).count();let rejected=d.jobs.iter().filter(|j|j.phase==JobPhase::Rejected).count();json!({"id":i,"label":LABELS[i],"nodes":d.nodes,"jobs":jobs,"queued":d.queued(),"running":d.running(),"completed":completed,"rejected":rejected,"offered":d.offered,"mean_wait_ms":d.mean_wait(),"node_seconds":d.node_seconds,"ready":d.count(Lifecycle::Ready),"starting":d.count(Lifecycle::Starting),"changes":d.changes})}).collect();
        json!({"at_ms":now,"duration_ms":self.replay.as_ref().map_or(HORIZON_MS,|r|r.end_ms),"paused":self.paused,"speed":self.speed,"scenario":self.scenario,"clients":self.clients,"settings":self.settings,"lanes":lanes,"history":self.history.iter().filter(|b|b.at_ms>=0).map(|b|json!({"at_ms":b.at_ms,"values":b.values})).collect::<Vec<_>>(),"trend":self.trend,"forecast":self.latest,"forecast_fresh":self.latest.as_ref().is_some_and(|f|f.fresh(now)),"forecast_status":if self.replay.is_some(){"Recorded replay".into()}else{self.forecaster.as_ref().map_or("Toto disconnected · reactive fallback".to_string(),|f|f.description())},"forecast_pending":self.pending_forecast.is_some(),"forecast_error":self.forecast_error,"forecast_calls":self.forecast_calls,"forecast_mean_latency_ms":self.forecast_latency/self.forecast_successes.max(1)as f64,"accuracy":self.accuracy,"jev_available":self.evaluator.is_some(),"jev_pending":self.pending_judge.is_some(),"jev_calls":self.jev_calls,"jev_limit":self.jev_settings.max_evaluations,"cost":self.cost.lock().unwrap().clone(),"replay":self.replay.is_some(),"decisions":self.decisions.iter().rev().take(60).collect::<Vec<_>>(),"error":self.error})
    }
    pub async fn command(&mut self, c: Command) -> Result<(), Error> {
        if self.replay.is_some()
            && !matches!(
                c,
                Command::Play
                    | Command::Pause
                    | Command::Step
                    | Command::Reset
                    | Command::Replay
                    | Command::Speed { .. }
            )
        {
            return Err(Error::Invalid("Reset to edit a recorded replay".into()));
        }
        let clients_changed = matches!(
            &c,
            Command::Client { .. } | Command::AddClient | Command::RemoveClient { .. }
        );
        match c {
            Command::Play => {
                self.paused = false;
            }
            Command::Pause => self.paused = true,
            Command::Step => {
                self.paused = true;
                self.advance().await?;
            }
            Command::Reset => self.reset()?,
            Command::Replay => {
                let tape = self
                    .replay
                    .clone()
                    .unwrap_or_else(|| self.recording.clone());
                self.settings = tape.settings.clone();
                self.clients = tape.clients.clone();
                self.scenario = tape.scenario;
                self.reset()?;
                self.history = tape.prehistory.clone();
                self.replay = Some(tape);
            }
            Command::Speed { value } => {
                if ![1, 2, 4, 10].contains(&value) {
                    return Err(Error::Invalid("Use 1, 2, 4 or 10× speed".into()));
                }
                self.speed = value;
            }
            Command::Scenario { scenario } => {
                self.scenario = scenario;
                self.reset()?;
            }
            Command::Client { client } => {
                if !client.valid() {
                    return Err(Error::Invalid("Invalid client configuration".into()));
                }
                let c = self
                    .clients
                    .iter_mut()
                    .find(|c| c.id == client.id)
                    .ok_or_else(|| Error::Invalid("Unknown client".into()))?;
                *c = client;
            }
            Command::AddClient => {
                if self.clients.len() >= 8 {
                    return Err(Error::Invalid("Eight clients maximum".into()));
                }
                let mut c = workload::defaults()[0].clone();
                c.id = (0..8)
                    .find(|id| self.clients.iter().all(|c| c.id != *id))
                    .unwrap();
                self.clients.push(c);
            }
            Command::RemoveClient { id } => {
                if self.clients.len() <= 1 || !self.clients.iter().any(|c| c.id == id) {
                    return Err(Error::Invalid("Retain at least one client".into()));
                }
                self.clients.retain(|c| c.id != id);
            }
            Command::Settings { settings } => {
                if !settings.valid()
                    || self
                        .engines
                        .iter()
                        .any(|e| e.data().active() > settings.max_nodes)
                {
                    return Err(Error::Invalid(
                        "Budget must cover active nodes in all three lanes; startup 5–120s".into(),
                    ));
                }
                let entry = Entry::Configure {
                    at_ms: self.at(),
                    settings,
                };
                self.apply_entry(&entry, false).await?;
                self.recording.entries.push(entry);
            }
        }
        if clients_changed {
            self.recording.entries.push(Entry::Clients {
                at_ms: self.at(),
                clients: self.clients.clone(),
            });
        }
        Ok(())
    }
    pub async fn tick(&mut self, ms: u64) -> Result<(), Error> {
        if self.paused {
            return Ok(());
        }
        self.remainder += ms * self.speed;
        while self.remainder >= 1000 && !self.paused {
            self.remainder -= 1000;
            self.advance().await?;
        }
        Ok(())
    }
    async fn apply_entry(&mut self, entry: &Entry, replay: bool) -> Result<(), Error> {
        match entry {
            Entry::Clients { clients, .. } => {
                self.clients = clients.clone();
            }
            Entry::Tick { bucket } => {
                if let Some(f) = self
                    .latest
                    .as_ref()
                    .filter(|f| f.origin_ms < bucket.at_ms as u64)
                {
                    let i = ((bucket.at_ms as u64 - f.origin_ms) / 1000 - 1) as usize;
                    if let Some(p) = f.series[1].median.get(i) {
                        self.accuracy.samples += 1;
                        self.accuracy.cpu_absolute_error +=
                            (*p as f64 - bucket.values[1] as f64).abs();
                        self.accuracy.cpu_covered += u64::from(
                            bucket.values[1] >= f.series[1].lower[i]
                                && bucket.values[1] <= f.series[1].upper[i],
                        );
                    }
                }
                for e in &mut self.engines {
                    e.event(Event::Tick(bucket.clone())).await?;
                }
                self.history.push(bucket.clone());
                self.trend.push(json!({"at_ms":bucket.at_ms,"ready_cpu":self.engines.iter().map(|e|e.data().count(Lifecycle::Ready)*8).collect::<Vec<_>>()}));
            }
            Entry::Configure { settings, .. } => {
                for e in &mut self.engines {
                    e.event(Event::Configure(settings.clone())).await?;
                }
                self.settings = settings.clone();
            }
            Entry::Forecast { record } => {
                if let Some(f) = &record.snapshot {
                    f.validate(&record.input).map_err(Error::Invalid)?;
                    self.forecast_latency += f.latency_ms;
                    self.forecast_successes += 1;
                    self.latest = Some(f.clone());
                }
                self.forecast_error = record.error.clone();
                if replay {
                    self.forecast_calls += 1;
                }
            }
            Entry::Decision { decision: d } => {
                let p = Proposal {
                    action: d.action,
                    observed_at: d.evidence.observed_at_ms,
                    revision: d.evidence.revision,
                    forecast_origin: d.evidence.forecast.as_ref().map(|f| f.origin_ms),
                };
                let applied = if let Some(error) = &d.result.error {
                    self.engines[d.lane].evaluation_error(error).await?;
                    false
                } else {
                    self.engines[d.lane].apply(p, d.result.confidence).await?.0
                };
                if applied != d.applied {
                    return Err(Error::Invalid("Replay guard outcome diverged".into()));
                }
                if replay && d.source == "jev" {
                    self.jev_calls += 1;
                }
                self.decisions.push(*d.clone());
            }
        }
        Ok(())
    }
    async fn decide(
        &mut self,
        lane: usize,
        evidence: Evidence,
        result: Inference,
        source: &str,
    ) -> Result<(), Error> {
        let action = result.choice.unwrap_or(Action::Hold);
        let p = Proposal {
            action,
            observed_at: evidence.observed_at_ms,
            revision: evidence.revision,
            forecast_origin: evidence.forecast.as_ref().map(|f| f.origin_ms),
        };
        let (applied, reason) = if let Some(error) = &result.error {
            self.engines[lane].evaluation_error(error).await?;
            (
                false,
                format!("Evaluation failed: {error}; capacity retained"),
            )
        } else {
            self.engines[lane].apply(p, result.confidence).await?
        };
        let d = Decision {
            at_ms: self.at(),
            lane,
            source: source.into(),
            evidence,
            result,
            action,
            applied,
            reason,
        };
        self.decisions.push(d.clone());
        self.recording.entries.push(Entry::Decision {
            decision: Box::new(d),
        });
        Ok(())
    }
    async fn thresholds(&mut self, lane: usize, forecast: bool, source: &str) -> Result<(), Error> {
        let e = judge::evidence(
            &self.engines[lane].data(),
            &self.history,
            if forecast { self.latest.as_ref() } else { None },
        );
        let a = judge::baseline(&e);
        let mut r = Inference::failed("");
        r.error = None;
        r.choice = Some(a);
        self.decide(lane, e, r, source).await
    }
    async fn live_policies(&mut self) -> Result<(), Error> {
        let now = self.at();
        if self
            .pending_forecast
            .as_ref()
            .is_some_and(|p| p.task.is_finished())
        {
            let mut p = self.pending_forecast.take().unwrap();
            let (input, result) = (&mut p.task)
                .await
                .map_err(|e| Error::Policy(e.to_string()))?;
            let result = result.and_then(|s| {
                s.validate(&input)?;
                Ok(s)
            });
            let (snapshot, error) = match result {
                Ok(s) => (Some(s), None),
                Err(e) => (None, Some(e)),
            };
            let entry = Entry::Forecast {
                record: ResultRecord {
                    input,
                    available_at_ms: now,
                    snapshot,
                    error,
                },
            };
            self.apply_entry(&entry, false).await?;
            self.recording.entries.push(entry);
        }
        if self
            .pending_judge
            .as_ref()
            .is_some_and(|p| p.task.is_finished())
        {
            let mut p = self.pending_judge.take().unwrap();
            let (e, r) = (&mut p.task)
                .await
                .map_err(|e| Error::Policy(e.to_string()))?;
            let failed = r.error.is_some();
            self.decide(2, e, r, "jev").await?;
            if failed {
                self.thresholds(2, false, "reactive fallback · Jev error")
                    .await?;
            }
        }
        // Every lane observes the same decision clock; model latency is recorded separately.
        let decision_due = self.last_baseline_sim.is_none_or(|t| now >= t + 10_000);
        if decision_due {
            self.last_baseline_sim = Some(now);
            self.thresholds(0, false, "reactive").await?;
            let fresh = self.latest.as_ref().is_some_and(|f| f.fresh(now));
            self.thresholds(
                1,
                true,
                if fresh {
                    "toto thresholds"
                } else {
                    "reactive fallback · forecast unavailable"
                },
            )
            .await?;
            if !fresh
                || self.evaluator.is_none()
                || self.jev_calls >= self.jev_settings.max_evaluations
            {
                self.thresholds(
                    2,
                    false,
                    if !fresh {
                        "reactive fallback · forecast unavailable"
                    } else {
                        "reactive fallback · Jev unavailable or budget exhausted"
                    },
                )
                .await?;
            }
        }
        if self.pending_forecast.is_none()
            && self.forecast_calls < 60
            && self.last_forecast_sim.is_none_or(|t| now >= t + 10_000)
            && self
                .last_forecast_wall
                .is_none_or(|t| t.elapsed() >= Duration::from_secs(5))
        {
            if let Some(f) = &self.forecaster {
                let f = f.clone();
                let input = workload::input(&self.history, now);
                input.validate().map_err(Error::Invalid)?;
                self.forecast_calls += 1;
                self.last_forecast_sim = Some(now);
                self.last_forecast_wall = Some(Instant::now());
                self.pending_forecast = Some(Pending {
                    task: tokio::spawn(async move {
                        let r =
                            tokio::time::timeout(Duration::from_secs(8), f.forecast(input.clone()))
                                .await
                                .unwrap_or_else(|_| Err("Toto deadline exceeded".into()));
                        (input, r)
                    }),
                });
            }
        }
        if decision_due
            && self.pending_judge.is_none()
            && self.jev_calls < self.jev_settings.max_evaluations
            && self.latest.as_ref().is_some_and(|f| f.fresh(now))
            && self.last_judge_sim.is_none_or(|t| now >= t + 10_000)
            && self
                .last_judge_wall
                .is_none_or(|t| t.elapsed() >= Duration::from_secs(1))
        {
            if let Some(j) = &self.evaluator {
                let j = j.clone();
                let e =
                    judge::evidence(&self.engines[2].data(), &self.history, self.latest.as_ref());
                if e.legal_choices.len() == 1 {
                    let mut result = Inference::failed("");
                    result.error = None;
                    result.choice = Some(e.legal_choices[0]);
                    self.last_judge_sim = Some(now);
                    self.decide(2, e, result, "deterministic · only legal action")
                        .await?;
                    return Ok(());
                }
                let cost = self.cost.clone();
                cost.lock().unwrap().dispatched();
                self.jev_calls += 1;
                self.last_judge_sim = Some(now);
                self.last_judge_wall = Some(Instant::now());
                self.pending_judge = Some(Pending {
                    task: tokio::spawn(async move {
                        let r = tokio::time::timeout(Duration::from_secs(2), j.evaluate(e.clone()))
                            .await
                            .unwrap_or_else(|_| Inference::failed("Jev deadline exceeded"));
                        cost.lock()
                            .unwrap()
                            .record_usage(r.model.as_deref(), r.usage.as_ref());
                        (e, r)
                    }),
                });
            }
        }
        Ok(())
    }
    async fn advance(&mut self) -> Result<(), Error> {
        if let Some(tape) = &self.replay {
            let target = (self.at() + 1000).min(tape.end_ms);
            while self
                .replay
                .as_ref()
                .unwrap()
                .entries
                .get(self.replay_cursor)
                .is_some_and(|e| e.at() <= target)
            {
                let entry = self.replay.as_ref().unwrap().entries[self.replay_cursor].clone();
                self.apply_entry(&entry, true).await?;
                self.replay_cursor += 1;
            }
            if self.at() >= self.replay.as_ref().unwrap().end_ms {
                self.paused = true;
            }
            return Ok(());
        }
        if self.at() >= HORIZON_MS {
            self.paused = true;
            return Ok(());
        }
        self.live_policies().await?;
        let bucket = workload::bucket(
            self.seed,
            self.scenario,
            &self.clients,
            (self.at() / 1000 + 1) as i64,
        );
        let entry = Entry::Tick { bucket };
        self.apply_entry(&entry, false).await?;
        self.recording.entries.push(entry);
        self.recording.end_ms = self.at();
        if self.at() >= HORIZON_MS {
            self.paused = true;
        }
        Ok(())
    }
}
#[cfg(test)]
mod tests;
