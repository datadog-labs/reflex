// Unless explicitly stated otherwise all files in this repository are licensed under the Apache-2.0 License.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-present Datadog, Inc.

use crate::telemetry::{scope, Telemetry};
use crate::{controller::valid_confidence, EvaluationError, ProposedDecision};
use futures_util::FutureExt;
use opentelemetry::metrics::Meter;
use std::{
    collections::VecDeque,
    future::Future,
    panic::{catch_unwind, AssertUnwindSafe},
    pin::Pin,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};
use thiserror::Error;
use tracing::Instrument;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{code}: {message}")]
pub struct Rejection {
    pub code: String,
    pub message: String,
}
impl Rejection {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}
#[derive(Debug, Clone)]
pub enum ExecutionOutcome<R> {
    Applied(R),
    Rejected {
        reason: Rejection,
        evaluation_error: Option<EvaluationError>,
    },
}
#[derive(Debug, Clone)]
pub struct TransitionReceipt<P> {
    pub from: P,
    pub to: P,
    /// True for a committed row, even when its source and target phase are equal.
    pub changed: bool,
    pub evaluation_error: Option<EvaluationError>,
    /// Ordered outcomes of post-commit work, including any completion transitions.
    pub effects: Vec<EffectOutcome<P>>,
}
#[derive(Debug, Clone)]
pub enum EffectOutcome<P> {
    Completed(ExecutionOutcome<TransitionReceipt<P>>),
    CompletionFailed(ExecutorError),
    Panicked,
    TimedOut,
    /// The next effect was not dispatched; its transition is already committed.
    ChainLimitReached,
}
#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum ExecutorError {
    #[error("state lock is poisoned")]
    Poisoned,
    #[error("a synchronous state-machine hook panicked; candidate was discarded")]
    HookPanicked,
    #[error("executor task could not complete")]
    TaskStopped,
}
#[derive(Debug, Error)]
pub enum MachineConfigError {
    #[error("initial state violates an invariant: {0}")]
    InvalidState(Rejection),
    #[error("invalid state-machine configuration: {0}")]
    InvalidConfiguration(String),
    #[error("an initialization hook panicked")]
    HookPanicked,
}

pub trait Executor {
    type Action;
    type Receipt;
    type Error;
    fn execute(
        &self,
        evaluation: Result<ProposedDecision<Self::Action>, EvaluationError>,
    ) -> impl Future<Output = Result<ExecutionOutcome<Self::Receipt>, Self::Error>> + Send;
}

/// Authoritative state. D::clone must create an isolated candidate: do not use
/// shared mutable handles in runtime data or perform I/O in synchronous hooks.
pub struct InMemory<P, D> {
    phase: P,
    data: D,
}
impl<P, D> InMemory<P, D> {
    pub fn new(phase: P, data: D) -> Self {
        Self { phase, data }
    }
}

#[doc(hidden)]
pub enum MachineInput<A, E> {
    Action(ProposedDecision<A>),
    EvaluationError(EvaluationError),
    Event(E),
}
impl<A, E> MachineInput<A, E> {
    fn evaluation_error(&self) -> Option<EvaluationError> {
        match self {
            Self::EvaluationError(e) => Some(e.clone()),
            _ => None,
        }
    }
}
#[doc(hidden)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum InputKind {
    Action,
    EvaluationError,
    Event,
}
#[doc(hidden)]
pub type EffectFuture<E> = Pin<Box<dyn Future<Output = E> + Send + 'static>>;
type Invariant<P, D> = Box<dyn Fn(&P, &D) -> Result<(), Rejection> + Send + Sync>;
type Matcher<A, E> = Box<dyn Fn(&MachineInput<A, E>) -> bool + Send + Sync>;
type Guard<D, A, E> =
    Box<dyn Fn(&D, &MachineInput<A, E>, Instant) -> Result<(), Rejection> + Send + Sync>;
type Update<D, A, E> = Box<
    dyn Fn(&mut D, &MachineInput<A, E>, Instant) -> Result<Option<EffectFuture<E>>, Rejection>
        + Send
        + Sync,
>;
type NoChange<A> = Box<dyn Fn(&A) -> bool + Send + Sync>;
#[doc(hidden)]
pub struct Transition<P, D, A, E> {
    pub from: P,
    pub to: Option<P>,
    pub kind: InputKind,
    pub matches: Matcher<A, E>,
    pub min_confidence: Option<f64>,
    pub guard: Guard<D, A, E>,
    pub update: Update<D, A, E>,
}
/// Usually constructed by [`crate::state_machine!`].
pub struct MachineDefinition<P, D, A, E> {
    pub invariants: Vec<Invariant<P, D>>,
    pub no_change: Option<NoChange<A>>,
    pub transitions: Vec<Transition<P, D, A, E>>,
}

pub struct StateMachineExecutor<P, D, A, E> {
    inner: Arc<Machine<P, D, A, E>>,
}
impl<P, D, A, E> Clone for StateMachineExecutor<P, D, A, E> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}
struct Machine<P, D, A, E> {
    definition: MachineDefinition<P, D, A, E>,
    state: Mutex<InMemory<P, D>>,
    clock: Box<dyn Fn() -> Instant + Send + Sync>,
    effect_timeout: Duration,
    max_effects: usize,
    next_id: AtomicU64,
    reports: Mutex<VecDeque<ExecutionReport<P>>>,
    report_capacity: usize,
    telemetry: Telemetry,
}
/// Completed calls are retained even if their awaiting caller is cancelled.
/// This bounded process-local history is not a durable audit log.
#[derive(Debug, Clone)]
pub struct ExecutionReport<P> {
    pub id: u64,
    pub outcome: Result<ExecutionOutcome<TransitionReceipt<P>>, ExecutorError>,
}
pub struct StateMachineBuilder<P, D, A, E> {
    definition: MachineDefinition<P, D, A, E>,
    store: Option<InMemory<P, D>>,
    clock: Box<dyn Fn() -> Instant + Send + Sync>,
    effect_timeout: Duration,
    max_effects: usize,
    report_capacity: usize,
    meter: Option<Meter>,
    name: Option<&'static str>,
}
impl<P, D, A, E> StateMachineExecutor<P, D, A, E> {
    pub fn builder(definition: MachineDefinition<P, D, A, E>) -> StateMachineBuilder<P, D, A, E> {
        StateMachineBuilder {
            definition,
            store: None,
            clock: Box::new(Instant::now),
            effect_timeout: Duration::from_secs(30),
            max_effects: 16,
            report_capacity: 64,
            meter: None,
            name: None,
        }
    }
}
impl<P, D, A, E> StateMachineBuilder<P, D, A, E>
where
    P: Clone + PartialEq + Send + Sync + 'static,
    D: Clone + Send + 'static,
    A: Send + 'static,
    E: Send + 'static,
{
    /// Optional stable telemetry label, shared by all clones of this executor.
    pub fn name(mut self, name: &'static str) -> Self {
        self.name = Some(name);
        self
    }
    /// Application-owned OpenTelemetry meter; defaults to the global provider at build time.
    pub fn meter(mut self, meter: Meter) -> Self {
        self.meter = Some(meter);
        self
    }
    pub fn store(mut self, store: InMemory<P, D>) -> Self {
        self.store = Some(store);
        self
    }
    pub fn clock(mut self, clock: impl Fn() -> Instant + Send + Sync + 'static) -> Self {
        self.clock = Box::new(clock);
        self
    }
    pub fn effect_timeout(mut self, timeout: Duration) -> Self {
        self.effect_timeout = timeout;
        self
    }
    pub fn max_effects(mut self, count: usize) -> Self {
        self.max_effects = count;
        self
    }
    pub fn report_capacity(mut self, count: usize) -> Self {
        self.report_capacity = count;
        self
    }
    pub fn build(self) -> Result<StateMachineExecutor<P, D, A, E>, MachineConfigError> {
        let invalid = |message: &str| MachineConfigError::InvalidConfiguration(message.into());
        let store = self.store.ok_or_else(|| invalid("store is required"))?;
        if self.effect_timeout.is_zero() || self.max_effects == 0 || self.report_capacity == 0 {
            return Err(invalid(
                "effect timeout, effect limit, and report capacity must be positive",
            ));
        }
        for row in &self.definition.transitions {
            if let Some(min) = row.min_confidence {
                if row.kind != InputKind::Action || !valid_confidence(min) {
                    return Err(invalid(
                        "confidence thresholds require an action row and a finite value in 0..=1",
                    ));
                }
            }
        }
        catch_unwind(AssertUnwindSafe(|| {
            for invariant in &self.definition.invariants {
                invariant(&store.phase, &store.data).map_err(MachineConfigError::InvalidState)?;
            }
            Ok(())
        }))
        .map_err(|_| MachineConfigError::HookPanicked)??;
        Ok(StateMachineExecutor {
            inner: Arc::new(Machine {
                definition: self.definition,
                state: Mutex::new(store),
                clock: self.clock,
                effect_timeout: self.effect_timeout,
                max_effects: self.max_effects,
                report_capacity: self.report_capacity,
                next_id: AtomicU64::new(1),
                reports: Mutex::new(VecDeque::new()),
                telemetry: Telemetry::new(self.meter, self.name, false),
            }),
        })
    }
}
type Prepared<P, E> = (
    ExecutionOutcome<TransitionReceipt<P>>,
    Option<EffectFuture<E>>,
);
impl<P, D, A, E> StateMachineExecutor<P, D, A, E>
where
    P: Clone + PartialEq + Send + Sync + 'static,
    D: Clone + Send + 'static,
    A: Send + 'static,
    E: Send + 'static,
{
    /// Clone current authoritative state under its lock. Treat the result as a
    /// detached read; all writes must be expressed as guarded inputs.
    pub fn state(&self) -> Result<(P, D), ExecutorError> {
        let state = self
            .inner
            .state
            .lock()
            .map_err(|_| ExecutorError::Poisoned)?;
        catch_unwind(AssertUnwindSafe(|| {
            (state.phase.clone(), state.data.clone())
        }))
        .map_err(|_| ExecutorError::HookPanicked)
    }
    pub fn recent_reports(&self) -> Vec<ExecutionReport<P>> {
        self.inner
            .reports
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .cloned()
            .collect()
    }
    /// Commits before effect dispatch. Waits for the bounded effect chain.
    /// Once first polled, caller cancellation does not cancel its supervisor.
    /// Runtime shutdown/process loss can still interrupt work after commit.
    pub async fn execute(
        &self,
        evaluation: Result<ProposedDecision<A>, EvaluationError>,
    ) -> Result<ExecutionOutcome<TransitionReceipt<P>>, ExecutorError> {
        self.submit(match evaluation {
            Ok(p) => MachineInput::Action(p),
            Err(e) => MachineInput::EvaluationError(e),
        })
        .await
    }
    pub async fn handle_event(
        &self,
        event: E,
    ) -> Result<ExecutionOutcome<TransitionReceipt<P>>, ExecutorError> {
        self.submit(MachineInput::Event(event)).await
    }
    async fn submit(
        &self,
        input: MachineInput<A, E>,
    ) -> Result<ExecutionOutcome<TransitionReceipt<P>>, ExecutorError> {
        let executor = self.clone();
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        // Create the guard before spawn so even a task dropped before first poll is observed.
        let mut operation = self.inner.telemetry.execution(id);
        let span = operation.span.clone();
        tokio::spawn(scope(
            async move {
                let outcome = executor.drive(input).await;
                let mut reports = executor
                    .inner
                    .reports
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                if reports.len() == executor.inner.report_capacity {
                    reports.pop_front();
                }
                reports.push_back(ExecutionReport {
                    id,
                    outcome: outcome.clone(),
                });
                drop(reports);
                operation.finish_execution(&outcome);
                outcome
            }
            .instrument(span),
        ))
        .await
        .map_err(|_| {
            tracing::error!(target: "reflex", machine = self.inner.telemetry.name,
                execution_id = id, error.type = "task_stopped", "Reflex supervisor could not complete");
            ExecutorError::TaskStopped
        })?
    }
    async fn drive(
        &self,
        input: MachineInput<A, E>,
    ) -> Result<ExecutionOutcome<TransitionReceipt<P>>, ExecutorError> {
        let (mut outcome, mut pending) = self.apply(input)?;
        if let ExecutionOutcome::Applied(receipt) = &mut outcome {
            let mut count = 0;
            while let Some(effect) = pending.take() {
                if count >= self.inner.max_effects {
                    tracing::warn!(target: "reflex", machine = self.inner.telemetry.name,
                        max_effects = self.inner.max_effects, "Reflex effect chain limit reached; next effect was not dispatched");
                    receipt.effects.push(EffectOutcome::ChainLimitReached);
                    break;
                }
                count += 1;
                let mut operation = self.inner.telemetry.effect(count);
                match tokio::time::timeout(
                    self.inner.effect_timeout,
                    AssertUnwindSafe(effect).catch_unwind(),
                )
                .instrument(operation.span.clone())
                .await
                {
                    Err(_) => {
                        operation.finish("timeout");
                        receipt.effects.push(EffectOutcome::TimedOut);
                        break;
                    }
                    Ok(Err(_)) => {
                        operation.finish("panicked");
                        receipt.effects.push(EffectOutcome::Panicked);
                        break;
                    }
                    Ok(Ok(event)) => {
                        operation.finish("completed");
                        // Effect timing ends before processing its separate completion input.
                        drop(operation);
                        match self.apply(MachineInput::Event(event)) {
                            Err(error) => {
                                receipt.effects.push(EffectOutcome::CompletionFailed(error));
                                break;
                            }
                            Ok((completion, next)) => {
                                receipt.effects.push(EffectOutcome::Completed(completion));
                                pending = next;
                            }
                        }
                    }
                }
            }
        }
        Ok(outcome)
    }
    fn apply(&self, input: MachineInput<A, E>) -> Result<Prepared<P, E>, ExecutorError> {
        let kind = match &input {
            MachineInput::Action(_) => "action",
            MachineInput::EvaluationError(_) => "evaluation_error",
            MachineInput::Event(_) => "event",
        };
        let mut stage = "execution";
        let result = self.apply_inner(input, &mut stage);
        // All instrumentation runs after the authoritative state lock is released.
        self.inner
            .telemetry
            .transition(result.as_ref().map(|(outcome, _)| outcome), kind, stage);
        result
    }
    fn apply_inner(
        &self,
        input: MachineInput<A, E>,
        stage: &mut &'static str,
    ) -> Result<Prepared<P, E>, ExecutorError> {
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| ExecutorError::Poisoned)?;
        // Catch hooks inside the protected scope, so a panic cannot poison the
        // mutex or install a partially updated candidate.
        catch_unwind(AssertUnwindSafe(|| {
            let evaluation_error = input.evaluation_error();
            let reject = |reason| {
                (
                    ExecutionOutcome::Rejected {
                        reason,
                        evaluation_error: evaluation_error.clone(),
                    },
                    None,
                )
            };
            let definition = &self.inner.definition;
            *stage = "current_invariant";
            for invariant in &definition.invariants {
                if let Err(reason) = invariant(&state.phase, &state.data) {
                    return reject(reason);
                }
            }
            *stage = "matching";
            let now = (self.inner.clock)();
            let no_change = match (&definition.no_change, &input) {
                (Some(matches), MachineInput::Action(p)) => matches(p.action()),
                _ => false,
            };
            let mut matching = definition
                .transitions
                .iter()
                .filter(|row| row.from == state.phase && (row.matches)(&input));
            let selected = matching.next();
            if matching.next().is_some() || (no_change && selected.is_some()) {
                return reject(Rejection::new(
                    "ambiguous_transition",
                    "More than one rule matches the current phase and input",
                ));
            }
            let unchanged_receipt = || TransitionReceipt {
                from: state.phase.clone(),
                to: state.phase.clone(),
                changed: false,
                evaluation_error: evaluation_error.clone(),
                effects: vec![],
            };
            if no_change {
                return (ExecutionOutcome::Applied(unchanged_receipt()), None);
            }
            let Some(row) = selected else {
                return reject(Rejection::new(
                    "undefined_transition",
                    "No rule matches the current phase and input",
                ));
            };
            *stage = "confidence";
            if let Some(min) = row.min_confidence {
                let confidence = match &input {
                    MachineInput::Action(p) => p.confidence(),
                    _ => None,
                };
                match confidence {
                    None => {
                        return reject(Rejection::new(
                            "missing_confidence",
                            "This transition requires model confidence",
                        ))
                    }
                    Some(v) if v < min => {
                        return reject(Rejection::new(
                            "insufficient_confidence",
                            "Confidence is below the transition threshold",
                        ))
                    }
                    _ => (),
                }
            }
            *stage = "guard";
            if let Err(reason) = (row.guard)(&state.data, &input, now) {
                return reject(reason);
            }
            let Some(target) = &row.to else {
                return (ExecutionOutcome::Applied(unchanged_receipt()), None);
            };
            *stage = "update";
            let mut candidate = state.data.clone();
            let effect = match (row.update)(&mut candidate, &input, now) {
                Ok(effect) => effect,
                Err(reason) => return reject(reason),
            };
            *stage = "candidate_invariant";
            for invariant in &definition.invariants {
                if let Err(reason) = invariant(target, &candidate) {
                    return reject(reason);
                }
            }
            *stage = "commit";
            // Construct every potentially fallible clone before installation.
            let receipt = TransitionReceipt {
                from: state.phase.clone(),
                to: target.clone(),
                changed: true,
                evaluation_error,
                effects: vec![],
            };
            let next = InMemory::new(target.clone(), candidate);
            *state = next;
            (ExecutionOutcome::Applied(receipt), effect)
        }))
        .map_err(|_| ExecutorError::HookPanicked)
    }
}
impl<P, D, A, E> Executor for StateMachineExecutor<P, D, A, E>
where
    P: Clone + PartialEq + Send + Sync + 'static,
    D: Clone + Send + 'static,
    A: Send + 'static,
    E: Send + 'static,
{
    type Action = A;
    type Receipt = TransitionReceipt<P>;
    type Error = ExecutorError;
    async fn execute(
        &self,
        evaluation: Result<ProposedDecision<A>, EvaluationError>,
    ) -> Result<ExecutionOutcome<Self::Receipt>, ExecutorError> {
        StateMachineExecutor::execute(self, evaluation).await
    }
}
