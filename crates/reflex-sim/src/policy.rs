// Unless explicitly stated otherwise all files in this repository are licensed under the Apache-2.0 License.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-present Datadog, Inc.

use crate::Error;
use reflex::{
    state_machine, ExecutionOutcome, InMemory, Judgment, Rejection, StateMachineExecutor,
};
use serde::{Deserialize, Serialize};
use std::{collections::VecDeque, future::Future, pin::Pin, time::Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CircuitPhase {
    Bypassed,
    Closed,
    Open,
    HalfOpen,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClientOutcome {
    Success,
    Error,
    Timeout,
    Shed,
}
#[derive(Debug, Clone, Copy)]
pub struct AdmissionContext {
    pub at_ms: f64,
    pub request_id: usize,
}
#[derive(Debug, Clone, Copy)]
pub enum Admission {
    Allow { generation: u64, probe: bool },
    Shed,
}
#[derive(Debug, Clone, Copy)]
pub struct Observation {
    pub at_ms: f64,
    pub request_id: usize,
    pub outcome: ClientOutcome,
    pub latency_ms: f64,
    pub generation: u64,
}
pub type PolicyFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, Error>> + Send + 'a>>;

/// One instance per downstream per run. The policy receives only client-visible
/// observations, never fault schedules, queue depth, random draws, or stress.
/// Awaited work must not sleep in wall time to represent a simulation delay.
pub trait Policy: Send {
    fn evidence(&self, _at_ms: f64) -> Option<crate::jev::Evidence> {
        None
    }
    fn apply_model(
        &mut self,
        _at_ms: f64,
        _evidence: crate::jev::Evidence,
        _result: crate::jev::Inference,
    ) -> PolicyFuture<'_, crate::jev::GuardResult> {
        Box::pin(async {
            Err(Error::Policy(
                "this policy does not accept model recommendations".into(),
            ))
        })
    }
    fn phase(&self) -> CircuitPhase;
    fn admit(&mut self, context: AdmissionContext) -> PolicyFuture<'_, Admission>;
    fn observe(&mut self, observation: Observation) -> PolicyFuture<'_, ()>;
}
#[derive(Clone, Copy)]
pub struct PolicyFactory {
    pub id: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub create: fn() -> Result<Box<dyn Policy>, Error>,
}
pub fn builtins() -> Vec<PolicyFactory> {
    vec![
        PolicyFactory{id:"unprotected",name:"Unprotected",description:"Every request reaches the downstream admission queue.",create:||Ok(Box::new(Unprotected))},
        PolicyFactory{id:"threshold",name:"Reflex · threshold",description:"A rolling error window opens the circuit; one probe tests recovery after cooldown. Implemented with the Reflex state machine.",create:||Ok(Box::new(ThresholdPolicy::new(ThresholdConfig::default())?))},
    ]
}
pub struct Unprotected;
impl Policy for Unprotected {
    fn phase(&self) -> CircuitPhase {
        CircuitPhase::Bypassed
    }
    fn admit(&mut self, _: AdmissionContext) -> PolicyFuture<'_, Admission> {
        Box::pin(async {
            Ok(Admission::Allow {
                generation: 0,
                probe: false,
            })
        })
    }
    fn observe(&mut self, _: Observation) -> PolicyFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThresholdConfig {
    pub window_ms: f64,
    pub minimum_samples: usize,
    pub failure_ratio: f64,
    pub cooldown_ms: f64,
}
impl Default for ThresholdConfig {
    fn default() -> Self {
        Self {
            window_ms: 5_000.0,
            minimum_samples: 20,
            failure_ratio: 0.5,
            cooldown_ms: 3_000.0,
        }
    }
}
#[derive(Clone)]
struct Data {
    config: ThresholdConfig,
    window: VecDeque<(f64, bool)>,
    until: Option<f64>,
    generation: u64,
}
#[derive(Clone, Copy)]
enum Action {
    Open { at: f64 },
    Probe { at: f64 },
}
trait Timed {
    fn at(&self) -> f64;
}
impl Timed for Action {
    fn at(&self) -> f64 {
        match self {
            Self::Open { at } | Self::Probe { at } => *at,
        }
    }
}
impl Timed for Observation {
    fn at(&self) -> f64 {
        self.at_ms
    }
}
fn invariant(p: &CircuitPhase, d: &Data) -> Result<(), Rejection> {
    if (*p == CircuitPhase::Open) == d.until.is_some() {
        Ok(())
    } else {
        Err(Rejection::new("phase_data", "cooldown and phase disagree"))
    }
}
fn current(d: &Data, o: &Observation, _: Instant) -> Result<(), Rejection> {
    if o.generation == d.generation {
        Ok(())
    } else {
        Err(Rejection::new(
            "stale",
            "observation belongs to an older circuit generation",
        ))
    }
}
fn record(d: &mut Data, o: &Observation, _: Instant) -> Result<(), Rejection> {
    while d
        .window
        .front()
        .is_some_and(|(at, _)| *at <= o.at_ms - d.config.window_ms)
    {
        d.window.pop_front();
    }
    d.window
        .push_back((o.at_ms, o.outcome != ClientOutcome::Success));
    Ok(())
}
fn distress(d: &Data, a: &Action, _: Instant) -> Result<(), Rejection> {
    let recent: Vec<_> = d
        .window
        .iter()
        .filter(|(at, _)| *at > a.at() - d.config.window_ms)
        .collect();
    if recent.len() >= d.config.minimum_samples
        && recent.iter().filter(|(_, bad)| *bad).count() as f64 / recent.len() as f64
            >= d.config.failure_ratio
    {
        Ok(())
    } else {
        Err(Rejection::new("healthy", "not enough sustained failures"))
    }
}
fn open<I: Timed>(d: &mut Data, i: &I, _: Instant) -> Result<(), Rejection> {
    d.generation = d
        .generation
        .checked_add(1)
        .ok_or_else(|| Rejection::new("generation", "generation exhausted"))?;
    d.until = Some(i.at() + d.config.cooldown_ms);
    d.window.clear();
    Ok(())
}
fn cooled(d: &Data, a: &Action, _: Instant) -> Result<(), Rejection> {
    if d.until.is_some_and(|until| a.at() >= until) {
        Ok(())
    } else {
        Err(Rejection::new("cooldown", "cooldown has not elapsed"))
    }
}
fn probe(d: &mut Data, _: &Action, _: Instant) -> Result<(), Rejection> {
    d.until = None;
    Ok(())
}
fn close(d: &mut Data, _: &Observation, _: Instant) -> Result<(), Rejection> {
    d.until = None;
    d.window.clear();
    Ok(())
}

pub struct ThresholdPolicy {
    machine: StateMachineExecutor<CircuitPhase, Data, Action, Observation>,
    phase: CircuitPhase,
}
impl ThresholdPolicy {
    pub fn new(config: ThresholdConfig) -> Result<Self, Error> {
        if !config.window_ms.is_finite()
            || config.window_ms <= 0.0
            || !config.cooldown_ms.is_finite()
            || config.cooldown_ms <= 0.0
            || config.minimum_samples == 0
            || !config.failure_ratio.is_finite()
            || !(0.0..=1.0).contains(&config.failure_ratio)
        {
            return Err(Error::Invalid(
                "invalid threshold breaker configuration".into(),
            ));
        }
        let definition = state_machine! {
            phase:CircuitPhase,data:Data,action:Action,event:Observation,invariants:[invariant],
            transitions:[
                CircuitPhase::Closed + action(Action::Open{..}) => CircuitPhase::Open {guard:distress,update:open},
                CircuitPhase::Open + action(Action::Probe{..}) => CircuitPhase::HalfOpen {guard:cooled,update:probe},
                CircuitPhase::Closed + event(_) => CircuitPhase::Closed {guard:current,update:record},
                CircuitPhase::Open + event(_) => unchanged {},
                CircuitPhase::HalfOpen + event(Observation{outcome:ClientOutcome::Success,..}) => CircuitPhase::Closed {guard:current,update:close},
                CircuitPhase::HalfOpen + event(Observation{outcome:ClientOutcome::Error|ClientOutcome::Timeout,..}) => CircuitPhase::Open {guard:current,update:open},
            ],
        };
        let machine = StateMachineExecutor::builder(definition)
            .store(InMemory::new(
                CircuitPhase::Closed,
                Data {
                    config,
                    window: VecDeque::new(),
                    until: None,
                    generation: 0,
                },
            ))
            .build()
            .map_err(|e| Error::Policy(e.to_string()))?;
        Ok(Self {
            machine,
            phase: CircuitPhase::Closed,
        })
    }
    async fn action(&mut self, a: Action) -> Result<bool, Error> {
        let outcome = self
            .machine
            .execute(
                Judgment {
                    action: a,
                    confidence: None,
                }
                .try_into(),
            )
            .await
            .map_err(|e| Error::Policy(e.to_string()))?;
        self.phase = self
            .machine
            .state()
            .map_err(|e| Error::Policy(e.to_string()))?
            .0;
        Ok(matches!(outcome, ExecutionOutcome::Applied(_)))
    }
}
impl Policy for ThresholdPolicy {
    fn phase(&self) -> CircuitPhase {
        self.phase
    }
    fn admit(&mut self, c: AdmissionContext) -> PolicyFuture<'_, Admission> {
        Box::pin(async move {
            let (_, d) = self
                .machine
                .state()
                .map_err(|e| Error::Policy(e.to_string()))?;
            let phase = self.phase;
            match phase {
                CircuitPhase::Closed => Ok(Admission::Allow {
                    generation: d.generation,
                    probe: false,
                }),
                CircuitPhase::Open if self.action(Action::Probe { at: c.at_ms }).await? => {
                    Ok(Admission::Allow {
                        generation: d.generation,
                        probe: true,
                    })
                }
                _ => Ok(Admission::Shed),
            }
        })
    }
    fn observe(&mut self, o: Observation) -> PolicyFuture<'_, ()> {
        Box::pin(async move {
            self.machine
                .handle_event(o)
                .await
                .map_err(|e| Error::Policy(e.to_string()))?;
            self.phase = self
                .machine
                .state()
                .map_err(|e| Error::Policy(e.to_string()))?
                .0;
            if self.phase == CircuitPhase::Closed {
                self.action(Action::Open { at: o.at_ms }).await?;
            }
            Ok(())
        })
    }
}
