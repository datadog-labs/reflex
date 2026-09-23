// Unless explicitly stated otherwise all files in this repository are licensed under the Apache-2.0 License.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-present Datadog, Inc.

use reflex::*;
use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
use tokio::sync::Notify;

#[derive(Debug, Clone, PartialEq, Eq)]
enum Phase {
    Idle,
    Busy,
    Done,
}
#[derive(Debug)]
enum Action {
    Start,
    Stay,
    Bad,
}
#[derive(Debug)]
enum Event {
    Complete,
    Cancel,
}
#[derive(Clone)]
struct Data {
    reserved: usize,
    started: Arc<Notify>,
    release: Arc<Notify>,
    effects: Arc<AtomicUsize>,
}
impl Default for Data {
    fn default() -> Self {
        Self {
            reserved: 0,
            started: Arc::new(Notify::new()),
            release: Arc::new(Notify::new()),
            effects: Arc::new(AtomicUsize::new(0)),
        }
    }
}
fn invariant(p: &Phase, d: &Data) -> Result<(), Rejection> {
    if (*p == Phase::Busy && d.reserved == 1) || (*p != Phase::Busy && d.reserved == 0) {
        Ok(())
    } else {
        Err(Rejection::new(
            "reservation",
            "phase and reservation disagree",
        ))
    }
}
fn reserve(d: &mut Data, _: &Action, _: Instant) -> Result<Data, Rejection> {
    d.reserved += 1;
    Ok(d.clone())
}
fn invalid_reserve(d: &mut Data, _: &Action, _: Instant) -> Result<Data, Rejection> {
    d.reserved = 2;
    Ok(d.clone())
}
fn clear(d: &mut Data, _: &Event, _: Instant) -> Result<(), Rejection> {
    d.reserved = 0;
    Ok(())
}
async fn effect(d: Data) -> Event {
    d.effects.fetch_add(1, Ordering::SeqCst);
    d.started.notify_one();
    d.release.notified().await;
    Event::Complete
}
fn definition() -> MachineDefinition<Phase, Data, Action, Event> {
    state_machine! {
        phase: Phase, data: Data, action: Action, event: Event,
        invariants: [invariant], no_change: Action::Stay,
        transitions: [
            Phase::Idle + action(Action::Start) => Phase::Busy { min_confidence: 0.8, update: reserve, effect: effect },
            Phase::Idle + action(Action::Bad) => Phase::Busy { update: invalid_reserve, effect: effect },
            Phase::Busy + event(Event::Complete) => Phase::Done { update: clear },
            Phase::Busy + event(Event::Cancel) => Phase::Idle { update: clear },
            Phase::Idle + evaluation_error(EvaluationError::Timeout) => unchanged {},
        ],
    }
}
type Machine = StateMachineExecutor<Phase, Data, Action, Event>;
fn machine(data: Data) -> Machine {
    StateMachineExecutor::builder(definition())
        .store(InMemory::new(Phase::Idle, data))
        .build()
        .unwrap()
}
fn proposal(
    a: Action,
    confidence: Option<f64>,
) -> Result<ProposedDecision<Action>, EvaluationError> {
    Judgment {
        action: a,
        confidence,
    }
    .try_into()
}
fn rejection<R>(outcome: ExecutionOutcome<R>) -> String {
    match outcome {
        ExecutionOutcome::Rejected { reason, .. } => reason.code,
        _ => panic!("expected rejection"),
    }
}
#[tokio::test]
async fn candidate_invariant_failure_discards_data_and_effect() {
    let data = Data::default();
    let counter = data.effects.clone();
    let ex = machine(data);
    assert_eq!(
        rejection(ex.execute(proposal(Action::Bad, Some(1.0))).await.unwrap()),
        "reservation"
    );
    assert_eq!(ex.state().unwrap().0, Phase::Idle);
    assert_eq!(ex.state().unwrap().1.reserved, 0);
    assert_eq!(counter.load(Ordering::SeqCst), 0);
}
#[test]
fn initial_invariants_and_configuration_are_checked() {
    let bad = Data {
        reserved: 2,
        ..Data::default()
    };
    assert!(matches!(
        StateMachineExecutor::builder(definition())
            .store(InMemory::new(Phase::Idle, bad))
            .build(),
        Err(MachineConfigError::InvalidState(_))
    ));
    for min in [f64::NAN, f64::INFINITY, -0.1, 1.1] {
        let mut def = definition();
        def.transitions[0].min_confidence = Some(min);
        assert!(StateMachineExecutor::builder(def)
            .store(InMemory::new(Phase::Idle, Data::default()))
            .build()
            .is_err());
    }
}
#[tokio::test]
async fn confidence_is_execution_policy_not_an_evaluation_error() {
    let ex = machine(Data::default());
    assert_eq!(
        rejection(ex.execute(proposal(Action::Start, None)).await.unwrap()),
        "missing_confidence"
    );
    assert_eq!(
        rejection(
            ex.execute(proposal(Action::Start, Some(0.79)))
                .await
                .unwrap()
        ),
        "insufficient_confidence"
    );
    assert_eq!(ex.state().unwrap().0, Phase::Idle);
}
#[tokio::test]
async fn effects_run_after_commit_outside_lock_and_complete_automatically() {
    let data = Data::default();
    let control = data.clone();
    let ex = machine(data);
    let runner = ex.clone();
    let call = tokio::spawn(async move {
        runner
            .execute(proposal(Action::Start, Some(0.8)))
            .await
            .unwrap()
    });
    control.started.notified().await;
    assert_eq!(ex.state().unwrap().0, Phase::Busy);
    control.release.notify_one();
    let ExecutionOutcome::Applied(receipt) = call.await.unwrap() else {
        panic!()
    };
    assert_eq!(receipt.from, Phase::Idle);
    assert_eq!(receipt.to, Phase::Busy);
    assert!(
        matches!(&receipt.effects[..], [EffectOutcome::Completed(ExecutionOutcome::Applied(r))] if r.to == Phase::Done)
    );
    assert_eq!(ex.state().unwrap().0, Phase::Done);
}
#[tokio::test]
async fn concurrent_proposals_reserve_only_once() {
    let data = Data::default();
    let control = data.clone();
    let ex = machine(data);
    let runner = ex.clone();
    let first =
        tokio::spawn(async move { runner.execute(proposal(Action::Start, Some(1.0))).await });
    control.started.notified().await;
    let mut calls = Vec::new();
    for _ in 0..20 {
        let runner = ex.clone();
        calls.push(tokio::spawn(async move {
            runner
                .execute(proposal(Action::Start, Some(1.0)))
                .await
                .unwrap()
        }));
    }
    for call in calls {
        assert_eq!(rejection(call.await.unwrap()), "undefined_transition");
    }
    assert_eq!(control.effects.load(Ordering::SeqCst), 1);
    control.release.notify_one();
    first.await.unwrap().unwrap();
}
#[tokio::test]
async fn cancelled_caller_does_not_lose_effect_completion_report() {
    let data = Data::default();
    let control = data.clone();
    let ex = machine(data);
    let runner = ex.clone();
    let call =
        tokio::spawn(async move { runner.execute(proposal(Action::Start, Some(1.0))).await });
    control.started.notified().await;
    call.abort();
    let _ = call.await;
    control.release.notify_one();
    tokio::time::timeout(Duration::from_secs(1), async {
        while ex.recent_reports().is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(ex.state().unwrap().0, Phase::Done);
    assert!(
        matches!(&ex.recent_reports()[0].outcome, Ok(ExecutionOutcome::Applied(r)) if r.effects.len() == 1)
    );
}
#[tokio::test]
async fn rejected_completion_is_visible_without_rolling_back_initiating_commit() {
    let data = Data::default();
    let control = data.clone();
    let ex = machine(data);
    let runner = ex.clone();
    let call = tokio::spawn(async move {
        runner
            .execute(proposal(Action::Start, Some(1.0)))
            .await
            .unwrap()
    });
    control.started.notified().await;
    ex.handle_event(Event::Cancel).await.unwrap();
    control.release.notify_one();
    let ExecutionOutcome::Applied(r) = call.await.unwrap() else {
        panic!()
    };
    assert!(r.changed);
    assert_eq!(r.to, Phase::Busy);
    assert!(
        matches!(&r.effects[..], [EffectOutcome::Completed(ExecutionOutcome::Rejected { reason, .. })] if reason.code == "undefined_transition")
    );
    assert_eq!(ex.state().unwrap().0, Phase::Idle);
}
#[tokio::test(start_paused = true)]
async fn effect_timeout_is_reported_after_commit() {
    let ex = StateMachineExecutor::builder(definition())
        .store(InMemory::new(Phase::Idle, Data::default()))
        .effect_timeout(Duration::from_millis(10))
        .build()
        .unwrap();
    let ExecutionOutcome::Applied(r) = ex
        .execute(proposal(Action::Start, Some(1.0)))
        .await
        .unwrap()
    else {
        panic!()
    };
    assert!(matches!(&r.effects[..], [EffectOutcome::TimedOut]));
    assert_eq!(ex.state().unwrap().0, Phase::Busy);
}
#[tokio::test]
async fn errors_retain_source_and_noop_is_explicit() {
    let ex = machine(Data::default());
    let ExecutionOutcome::Applied(r) = ex.execute(Err(EvaluationError::Timeout)).await.unwrap()
    else {
        panic!()
    };
    assert!(!r.changed);
    assert_eq!(r.evaluation_error, Some(EvaluationError::Timeout));
    let err = EvaluationError::Judge(JudgeError::new("http", "HTTP 401"));
    assert!(
        matches!(ex.execute(Err(err.clone())).await.unwrap(), ExecutionOutcome::Rejected { evaluation_error: Some(e), .. } if e == err)
    );
    let ExecutionOutcome::Applied(r) = ex.execute(proposal(Action::Stay, None)).await.unwrap()
    else {
        panic!()
    };
    assert!(!r.changed);
    assert!(r.effects.is_empty());
}
#[tokio::test]
async fn overlapping_matches_reject_before_guards() {
    fn must_not_run(_: &Data, _: &Action, _: Instant) -> Result<(), Rejection> {
        panic!("guard should not run")
    }
    let def = state_machine! {
        phase: Phase, data: Data, action: Action, event: Event,
        transitions: [
            Phase::Idle + action(Action::Start) => unchanged { guard: must_not_run },
            Phase::Idle + action(_) => unchanged {},
        ],
    };
    let ex = StateMachineExecutor::builder(def)
        .store(InMemory::new(Phase::Idle, Data::default()))
        .build()
        .unwrap();
    assert_eq!(
        rejection(
            ex.execute(proposal(Action::Start, Some(1.0)))
                .await
                .unwrap()
        ),
        "ambiguous_transition"
    );
}
#[tokio::test]
async fn nochange_cannot_mask_overlapping_row() {
    let mut def = definition();
    def.no_change = Some(Box::new(|a| matches!(a, Action::Start)));
    let ex = StateMachineExecutor::builder(def)
        .store(InMemory::new(Phase::Idle, Data::default()))
        .build()
        .unwrap();
    assert_eq!(
        rejection(
            ex.execute(proposal(Action::Start, Some(1.0)))
                .await
                .unwrap()
        ),
        "ambiguous_transition"
    );
}
#[tokio::test]
async fn hook_panic_discards_candidate_and_does_not_poison_store() {
    let mut def = definition();
    def.transitions[0].update = Box::new(|d, _, _| {
        d.reserved = 99;
        panic!("bad application hook")
    });
    let ex = StateMachineExecutor::builder(def)
        .store(InMemory::new(Phase::Idle, Data::default()))
        .build()
        .unwrap();
    assert_eq!(
        ex.execute(proposal(Action::Start, Some(1.0)))
            .await
            .unwrap_err(),
        ExecutorError::HookPanicked
    );
    assert_eq!(ex.state().unwrap().1.reserved, 0);
    assert!(matches!(
        ex.execute(proposal(Action::Stay, None)).await.unwrap(),
        ExecutionOutcome::Applied(_)
    ));
}
#[tokio::test]
async fn effect_panic_preserves_commit() {
    let mut def = definition();
    def.transitions[0].update = Box::new(|d, _, _| {
        d.reserved = 1;
        Ok(Some(Box::pin(async { panic!("effect failed") })))
    });
    let ex = StateMachineExecutor::builder(def)
        .store(InMemory::new(Phase::Idle, Data::default()))
        .build()
        .unwrap();
    let ExecutionOutcome::Applied(r) = ex
        .execute(proposal(Action::Start, Some(1.0)))
        .await
        .unwrap()
    else {
        panic!()
    };
    assert!(matches!(&r.effects[..], [EffectOutcome::Panicked]));
    assert_eq!(ex.state().unwrap().0, Phase::Busy);
}
#[tokio::test]
async fn report_history_is_bounded() {
    let ex = StateMachineExecutor::builder(definition())
        .store(InMemory::new(Phase::Idle, Data::default()))
        .report_capacity(2)
        .build()
        .unwrap();
    for _ in 0..3 {
        ex.execute(proposal(Action::Stay, None)).await.unwrap();
    }
    let reports = ex.recent_reports();
    assert_eq!(reports.len(), 2);
    assert_eq!(reports[0].id, 2);
    assert_eq!(reports[1].id, 3);
}
#[tokio::test]
async fn guard_reads_current_time_and_data() {
    let now = Instant::now();
    let def = state_machine! {
        phase: Phase, data: Instant, action: Action, event: (),
        transitions: [Phase::Idle + action(Action::Start) => Phase::Done {
            guard: |deadline: &Instant, _: &Action, now: Instant| if now >= *deadline { Ok(()) } else { Err(Rejection::new("early", "deadline has not elapsed")) },
        }],
    };
    let ex = StateMachineExecutor::builder(def)
        .store(InMemory::new(Phase::Idle, now + Duration::from_secs(1)))
        .clock(move || now)
        .build()
        .unwrap();
    assert_eq!(
        rejection(
            ex.execute(proposal(Action::Start, Some(1.0)))
                .await
                .unwrap()
        ),
        "early"
    );
}
#[tokio::test]
async fn bounded_effect_chains_report_undispatched_work() {
    let def = state_machine! {
        phase: Phase, data: (), action: Action, event: (),
        transitions: [
            Phase::Idle + action(Action::Start) => Phase::Busy { update: |_: &mut (), _: &Action, _| Ok(()), effect: |()| async {} },
            Phase::Busy + event(()) => Phase::Busy { update: |_: &mut (), _: &(), _| Ok(()), effect: |()| async {} },
        ],
    };
    let ex = StateMachineExecutor::builder(def)
        .store(InMemory::new(Phase::Idle, ()))
        .max_effects(2)
        .build()
        .unwrap();
    let ExecutionOutcome::Applied(r) = ex.execute(proposal(Action::Start, None)).await.unwrap()
    else {
        panic!()
    };
    assert_eq!(r.effects.len(), 3);
    assert!(matches!(
        r.effects.last(),
        Some(EffectOutcome::ChainLimitReached)
    ));
}

#[tokio::test]
async fn hooks_can_own_application_dependencies() {
    let calls = Arc::new(AtomicUsize::new(0));
    let observer = calls.clone();
    let allowed = Arc::new(true);
    let def = state_machine! {
        phase: Phase, data: (), action: Action, event: (),
        transitions: [
            Phase::Idle + action(Action::Start) => Phase::Busy {
                guard: move |_: &(), _: &Action, _| if *allowed { Ok(()) } else { Err(Rejection::new("no", "not allowed")) },
                update: |_: &mut (), _: &Action, _| Ok(()),
                effect: move |()| { let calls = calls.clone(); async move { calls.fetch_add(1, Ordering::SeqCst); } },
            },
            Phase::Busy + event(()) => Phase::Done {},
        ],
    };
    let ex = StateMachineExecutor::builder(def)
        .store(InMemory::new(Phase::Idle, ()))
        .build()
        .unwrap();
    ex.execute(proposal(Action::Start, None)).await.unwrap();
    assert_eq!(observer.load(Ordering::SeqCst), 1);
    assert_eq!(ex.state().unwrap().0, Phase::Done);
}

#[tokio::test]
async fn current_invariants_are_rechecked_before_noop_or_update() {
    let valid = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let control = valid.clone();
    let mut def = definition();
    def.invariants.push(Box::new(move |_, _| {
        if valid.load(Ordering::SeqCst) {
            Ok(())
        } else {
            Err(Rejection::new(
                "current_invalid",
                "policy constraint changed",
            ))
        }
    }));
    let ex = StateMachineExecutor::builder(def)
        .store(InMemory::new(Phase::Idle, Data::default()))
        .build()
        .unwrap();
    control.store(false, Ordering::SeqCst);
    assert_eq!(
        rejection(ex.execute(proposal(Action::Stay, None)).await.unwrap()),
        "current_invalid"
    );
    assert_eq!(
        rejection(
            ex.execute(proposal(Action::Start, Some(1.0)))
                .await
                .unwrap()
        ),
        "current_invalid"
    );
    assert_eq!(ex.state().unwrap().0, Phase::Idle);
}

#[tokio::test]
async fn failed_update_rolls_back_its_partial_candidate() {
    let mut def = definition();
    def.transitions[0].update = Box::new(|d, _, _| {
        d.reserved = 100;
        Err(Rejection::new("update_failed", "cannot allocate resource"))
    });
    let ex = StateMachineExecutor::builder(def)
        .store(InMemory::new(Phase::Idle, Data::default()))
        .build()
        .unwrap();
    assert_eq!(
        rejection(
            ex.execute(proposal(Action::Start, Some(1.0)))
                .await
                .unwrap()
        ),
        "update_failed"
    );
    assert_eq!(ex.state().unwrap().1.reserved, 0);
}
