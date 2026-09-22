use futures_util::FutureExt;
use opentelemetry::{metrics::MeterProvider, trace::TracerProvider, KeyValue};
use opentelemetry_sdk::{
    metrics::{
        data::{AggregatedMetrics, MetricData},
        InMemoryMetricExporter, PeriodicReader, SdkMeterProvider,
    },
    trace::{InMemorySpanExporter, Sampler, SdkTracerProvider},
};
use reflex::*;
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::Notify;
use tracing::{instrument::WithSubscriber, Instrument};
use tracing_subscriber::{filter::Targets, prelude::*};

struct Capture {
    meter: SdkMeterProvider,
    metrics: InMemoryMetricExporter,
    tracer: SdkTracerProvider,
    spans: InMemorySpanExporter,
    dispatch: tracing::Dispatch,
}
impl Capture {
    fn new(sampled: bool) -> Self {
        let metrics = InMemoryMetricExporter::default();
        let meter = SdkMeterProvider::builder()
            .with_reader(
                PeriodicReader::builder(metrics.clone())
                    .with_interval(Duration::from_secs(3600))
                    .build(),
            )
            .build();
        let spans = InMemorySpanExporter::default();
        let tracer = SdkTracerProvider::builder()
            .with_simple_exporter(spans.clone())
            .with_sampler(if sampled {
                Sampler::AlwaysOn
            } else {
                Sampler::AlwaysOff
            })
            .build();
        let dispatch = tracing::Dispatch::new(
            tracing_subscriber::registry()
                .with(tracing_opentelemetry::layer().with_tracer(tracer.tracer("reflex-test")))
                .with(Targets::new().with_target("reflex", tracing::Level::TRACE)),
        );
        Self {
            meter,
            metrics,
            tracer,
            spans,
            dispatch,
        }
    }
    fn count(&self, metric: &str, status: &str) -> u64 {
        self.meter.force_flush().unwrap();
        let batches = self.metrics.get_finished_metrics().unwrap();
        batches
            .last()
            .into_iter()
            .flat_map(|r| r.scope_metrics())
            .flat_map(|s| s.metrics())
            .filter(|m| m.name() == metric)
            .map(|m| match m.data() {
                AggregatedMetrics::U64(MetricData::Sum(sum)) => sum
                    .data_points()
                    .filter(|p| {
                        p.attributes()
                            .any(|a| a.key.as_str() == "status" && a.value.to_string() == status)
                    })
                    .map(|p| p.value())
                    .sum::<u64>(),
                _ => panic!("unexpected metric"),
            })
            .sum()
    }
    fn assert_minimal(&self) {
        self.meter.force_flush().unwrap();
        let batches = self.metrics.get_finished_metrics().unwrap();
        for metric in batches
            .last()
            .unwrap()
            .scope_metrics()
            .flat_map(|s| s.metrics())
        {
            assert!(["reflex.evaluations", "reflex.transitions"].contains(&metric.name()));
            if let AggregatedMetrics::U64(MetricData::Sum(sum)) = metric.data() {
                for point in sum.data_points() {
                    assert!(point
                        .attributes()
                        .all(|a| ["status", "controller", "machine"].contains(&a.key.as_str())));
                }
            }
        }
        let captured = format!(
            "{:?}{:?}",
            batches,
            self.spans.get_finished_spans().unwrap()
        );
        assert!(!captured.contains("private error message"));
    }
}
impl Drop for Capture {
    fn drop(&mut self) {
        self.meter.shutdown().unwrap();
        self.tracer.shutdown().unwrap();
    }
}
fn attr(attrs: &[KeyValue], key: &str) -> Option<String> {
    attrs
        .iter()
        .find(|a| a.key.as_str() == key)
        .map(|a| a.value.to_string())
}
struct TestJudge;
impl Judge<u8, ()> for TestJudge {
    async fn judge(&self, state: &u8) -> Result<Judgment<()>, JudgeError> {
        async {
            match state {
                0 => Ok(Judgment {
                    action: (),
                    confidence: Some(0.8),
                }),
                1 => Err(JudgeError::new("provider_failure", "private error message")),
                2 => Ok(Judgment {
                    action: (),
                    confidence: Some(f64::NAN),
                }),
                3 => std::future::pending().await,
                _ => panic!("test judge panic"),
            }
        }
        .instrument(tracing::info_span!(target:"reflex", "provider.test"))
        .await
    }
}
#[tokio::test]
async fn evaluations_count_errors_cancellation_and_preserve_provider_parentage() {
    let c = Capture::new(true);
    let controller = Controller::builder()
        .name("test")
        .meter(c.meter.meter("reflex"))
        .judge(TestJudge)
        .inference_timeout(Duration::from_millis(30))
        .build()
        .unwrap();
    for state in 0..4 {
        let _ = controller
            .evaluate(&state)
            .with_subscriber(c.dispatch.clone())
            .await;
    }
    let mut future = Box::pin(controller.evaluate(&3).with_subscriber(c.dispatch.clone()));
    assert!(futures_util::poll!(future.as_mut()).is_pending());
    drop(future);
    let _ =
        std::panic::AssertUnwindSafe(controller.evaluate(&4).with_subscriber(c.dispatch.clone()))
            .catch_unwind()
            .await;
    assert_eq!(c.count("reflex.evaluations", "proposed"), 1);
    assert_eq!(c.count("reflex.evaluations", "error"), 4);
    assert_eq!(c.count("reflex.evaluations", "cancelled"), 1);
    let spans = c.spans.get_finished_spans().unwrap();
    for provider in spans.iter().filter(|s| s.name == "provider.test") {
        assert!(spans
            .iter()
            .any(|s| s.name == "reflex.evaluate"
                && s.span_context.span_id() == provider.parent_span_id));
    }
    let kinds: Vec<_> = spans
        .iter()
        .filter_map(|s| attr(&s.attributes, "error.type"))
        .collect();
    for kind in [
        "timeout",
        "judge_error",
        "invalid_confidence",
        "cancelled",
        "panicked",
    ] {
        assert!(kinds.contains(&kind.to_owned()), "missing {kind}");
    }
    c.assert_minimal();
}

// No Debug/Serialize bounds are imposed on application types by telemetry.
#[derive(Clone, PartialEq)]
enum Phase {
    Idle,
    Busy,
}
enum Action {
    Commit,
    Stay,
    Guard,
    BadCandidate,
    BadUpdate,
    Panic,
    Confidence,
    Unknown,
    Effect,
}
enum Event {
    Done,
    Again,
    Unknown,
}
#[derive(Clone, Copy)]
enum Mode {
    Complete,
    Wait,
    Timeout,
    Panic,
    Chain,
    RejectCompletion,
}
#[derive(Clone)]
struct Data {
    value: usize,
    mode: Mode,
    started: Arc<Notify>,
    release: Arc<Notify>,
}
fn invariant(_: &Phase, d: &Data) -> Result<(), Rejection> {
    if d.value < 10 {
        Ok(())
    } else {
        Err(Rejection::new("too_large", "private error message"))
    }
}
fn increment(d: &mut Data, _: &Action, _: Instant) -> Result<(), Rejection> {
    d.value += 1;
    Ok(())
}
fn bad_candidate(d: &mut Data, _: &Action, _: Instant) -> Result<(), Rejection> {
    d.value = 99;
    Ok(())
}
fn bad_update(_: &mut Data, _: &Action, _: Instant) -> Result<(), Rejection> {
    Err(Rejection::new("bad_update", "private error message"))
}
fn panic_update(_: &mut Data, _: &Action, _: Instant) -> Result<(), Rejection> {
    panic!("test hook panic")
}
fn guard(_: &Data, _: &Action, _: Instant) -> Result<(), Rejection> {
    Err(Rejection::new("policy", "private error message"))
}
fn prepare(d: &mut Data, _: &Action, _: Instant) -> Result<Data, Rejection> {
    Ok(d.clone())
}
fn repeat(d: &mut Data, _: &Event, _: Instant) -> Result<Data, Rejection> {
    Ok(d.clone())
}
async fn effect(d: Data) -> Event {
    match d.mode {
        Mode::Complete => Event::Done,
        Mode::Wait => {
            d.started.notify_one();
            d.release.notified().await;
            Event::Done
        }
        Mode::Timeout => std::future::pending().await,
        Mode::Panic => panic!("test effect panic"),
        Mode::Chain => Event::Again,
        Mode::RejectCompletion => Event::Unknown,
    }
}
type Machine = StateMachineExecutor<Phase, Data, Action, Event>;
fn machine(c: &Capture, mode: Mode) -> (Machine, Data) {
    let data = Data {
        value: 0,
        mode,
        started: Arc::new(Notify::new()),
        release: Arc::new(Notify::new()),
    };
    let definition = state_machine! {
        phase:Phase,data:Data,action:Action,event:Event,
        invariants:[invariant],no_change:Action::Stay,
        transitions:[
            Phase::Idle + action(Action::Commit) => Phase::Idle {update:increment},
            Phase::Idle + action(Action::Guard) => Phase::Busy {guard:guard},
            Phase::Idle + action(Action::BadCandidate) => Phase::Busy {update:bad_candidate},
            Phase::Idle + action(Action::BadUpdate) => Phase::Busy {update:bad_update},
            Phase::Idle + action(Action::Panic) => Phase::Busy {update:panic_update},
            Phase::Idle + action(Action::Confidence) => Phase::Busy {min_confidence:0.9},
            Phase::Idle + action(Action::Effect) => Phase::Busy {update:prepare,effect:effect},
            Phase::Idle + evaluation_error(EvaluationError::Timeout) => unchanged {},
            Phase::Busy + event(Event::Done) => Phase::Idle {},
            Phase::Busy + event(Event::Again) => Phase::Busy {update:repeat,effect:effect},
        ],
    };
    let machine = StateMachineExecutor::builder(definition)
        .name("test")
        .meter(c.meter.meter("reflex"))
        .store(InMemory::new(Phase::Idle, data.clone()))
        .max_effects(2)
        .effect_timeout(if matches!(mode, Mode::Wait) {
            Duration::from_secs(5)
        } else {
            Duration::from_millis(50)
        })
        .build()
        .unwrap();
    (machine, data)
}
fn proposal(action: Action) -> Result<ProposedDecision<Action>, EvaluationError> {
    Judgment {
        action,
        confidence: Some(0.8),
    }
    .try_into()
}
#[tokio::test]
async fn transitions_count_once_with_rejection_details_in_events_only() {
    let c = Capture::new(true);
    let (ex, _) = machine(&c, Mode::Complete);
    for action in [
        Action::Commit,
        Action::Stay,
        Action::Guard,
        Action::BadCandidate,
        Action::BadUpdate,
        Action::Panic,
        Action::Confidence,
        Action::Unknown,
    ] {
        let _ = ex
            .execute(proposal(action))
            .with_subscriber(c.dispatch.clone())
            .await;
    }
    ex.execute(Err(EvaluationError::Timeout))
        .with_subscriber(c.dispatch.clone())
        .await
        .unwrap();
    assert_eq!(c.count("reflex.transitions", "applied"), 1); // committed same-phase update
    assert_eq!(c.count("reflex.transitions", "unchanged"), 2);
    assert_eq!(c.count("reflex.transitions", "rejected"), 5);
    assert_eq!(c.count("reflex.transitions", "error"), 1);
    let spans = c.spans.get_finished_spans().unwrap();
    let events: Vec<_> = spans.iter().flat_map(|s| s.events.iter()).collect();
    for stage in [
        "guard",
        "candidate_invariant",
        "update",
        "confidence",
        "matching",
    ] {
        let event = events
            .iter()
            .find(|e| {
                attr(&e.attributes, "stage").as_deref() == Some(stage)
                    && attr(&e.attributes, "status").as_deref() == Some("rejected")
            })
            .unwrap();
        assert_eq!(attr(&event.attributes, "level").as_deref(), Some("DEBUG"));
    }
    assert_eq!(ex.state().unwrap().1.value, 1);
    c.assert_minimal();
}
#[tokio::test]
async fn effect_outcomes_do_not_reclassify_committed_transitions() {
    for (mode, status, applied, rejected) in [
        (Mode::Complete, "completed", 2, 0),
        (Mode::Timeout, "timeout", 1, 0),
        (Mode::Panic, "panicked", 1, 0),
        (Mode::Chain, "completed", 3, 0),
        (Mode::RejectCompletion, "completed", 1, 1),
    ] {
        let c = Capture::new(true);
        let (ex, _) = machine(&c, mode);
        let result = ex
            .execute(proposal(Action::Effect))
            .with_subscriber(c.dispatch.clone())
            .await
            .unwrap();
        assert!(matches!(result, ExecutionOutcome::Applied(_)));
        assert_eq!(c.count("reflex.transitions", "applied"), applied);
        assert_eq!(c.count("reflex.transitions", "rejected"), rejected);
        assert_eq!(c.count("reflex.transitions", "error"), 0);
        let spans = c.spans.get_finished_spans().unwrap();
        let root = spans.iter().find(|s| s.name == "reflex.execute").unwrap();
        let effects: Vec<_> = spans.iter().filter(|s| s.name == "reflex.effect").collect();
        assert_eq!(
            effects.len(),
            if matches!(mode, Mode::Chain) { 2 } else { 1 }
        );
        for effect in effects {
            assert_eq!(effect.parent_span_id, root.span_context.span_id());
            assert_eq!(attr(&effect.attributes, "status").as_deref(), Some(status));
            assert!(effect
                .events
                .iter()
                .any(|e| attr(&e.attributes, "duration_s").is_some()));
        }
        if matches!(mode, Mode::Chain) {
            assert!(root
                .events
                .iter()
                .any(|e| e.name.contains("chain limit reached")));
        }
        c.assert_minimal();
    }
}
#[tokio::test]
async fn caller_cancellation_does_not_cancel_supervised_execution_or_lose_trace_context() {
    let c = Capture::new(true);
    let (ex, data) = machine(&c, Mode::Wait);
    let copied = ex.clone();
    let parent = tracing::dispatcher::with_default(
        &c.dispatch,
        || tracing::info_span!(target:"reflex","test.parent"),
    );
    let parent_id = parent.id();
    assert!(parent_id.is_some());
    let caller = tokio::spawn(
        async move { copied.execute(proposal(Action::Effect)).await }
            .instrument(parent)
            .with_subscriber(c.dispatch.clone()),
    );
    data.started.notified().await;
    caller.abort();
    assert!(matches!(caller.await, Err(error) if error.is_cancelled()));
    assert_eq!(c.count("reflex.transitions", "applied"), 1);
    data.release.notify_one();
    tokio::time::timeout(Duration::from_secs(1), async {
        while ex.recent_reports().is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(c.count("reflex.transitions", "applied"), 2);
    let spans = c.spans.get_finished_spans().unwrap();
    let root = spans.iter().find(|s| s.name == "reflex.execute").unwrap();
    let effect = spans.iter().find(|s| s.name == "reflex.effect").unwrap();
    assert_eq!(attr(&root.attributes, "status").as_deref(), Some("applied"));
    assert_eq!(effect.span_context.trace_id(), root.span_context.trace_id());
    assert_eq!(
        attr(&effect.attributes, "status").as_deref(),
        Some("completed")
    );
    assert!(!root.events.iter().any(|e| e.name.contains("interrupted")));
}
#[tokio::test]
async fn counters_are_independent_of_trace_sampling() {
    let c = Capture::new(false);
    let controller = Controller::builder()
        .judge(TestJudge)
        .meter(c.meter.meter("reflex"))
        .build()
        .unwrap();
    controller
        .evaluate(&0)
        .with_subscriber(c.dispatch.clone())
        .await
        .unwrap();
    let (ex, _) = machine(&c, Mode::Complete);
    ex.execute(proposal(Action::Commit))
        .with_subscriber(c.dispatch.clone())
        .await
        .unwrap();
    assert_eq!(c.count("reflex.evaluations", "proposed"), 1);
    assert_eq!(c.count("reflex.transitions", "applied"), 1);
    assert!(c.spans.get_finished_spans().unwrap().is_empty());
    c.assert_minimal();
}

#[test]
fn runtime_shutdown_records_effect_cancellation_without_recounting_the_commit() {
    let c = Capture::new(true);
    let (ex, data) = machine(&c, Mode::Wait);
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let supervisor_caller = runtime.spawn(
        async move { ex.execute(proposal(Action::Effect)).await }
            .with_subscriber(c.dispatch.clone()),
    );
    runtime.block_on(data.started.notified());
    drop(runtime);
    drop(supervisor_caller);
    assert_eq!(c.count("reflex.transitions", "applied"), 1);
    assert_eq!(c.count("reflex.transitions", "error"), 0);
    let spans = c.spans.get_finished_spans().unwrap();
    let effect = spans.iter().find(|s| s.name == "reflex.effect").unwrap();
    assert_eq!(
        attr(&effect.attributes, "status").as_deref(),
        Some("cancelled")
    );
    assert!(effect
        .events
        .iter()
        .any(|e| attr(&e.attributes, "duration_s").is_some()));
}
