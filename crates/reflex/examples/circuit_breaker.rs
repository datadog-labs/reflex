//! Offline executable example: replace DemoJudge with TypeSafeJudge for Jev.
use reflex::*;
use std::time::{Duration, Instant};

#[derive(Clone, Debug, PartialEq, Eq)]
enum Phase {
    Closed,
    Open,
    HalfOpen,
}
#[derive(Clone)]
struct CircuitData {
    cooldown_until: Option<Instant>,
    active_probe: Option<u64>,
    next_probe_id: u64,
    completed_requests: u64,
    timeouts: u64,
}
#[derive(Debug)]
enum CircuitAction {
    Open,
    PermitProbe,
    NoChange,
}
enum ProbeEvent {
    Succeeded { probe_id: u64 },
    Failed { probe_id: u64 },
}
struct ReservedProbe {
    probe_id: u64,
}
struct CircuitState {
    phase: Phase,
    can_probe: bool,
    completed_requests: u64,
    timeouts: u64,
}
struct DemoJudge;
impl Judge<CircuitState, CircuitAction> for DemoJudge {
    async fn judge(&self, state: &CircuitState) -> Result<Judgment<CircuitAction>, JudgeError> {
        let action = match state.phase {
            Phase::Closed
                if state.completed_requests >= 20
                    && state.timeouts * 2 >= state.completed_requests =>
            {
                CircuitAction::Open
            }
            Phase::Open if state.can_probe => CircuitAction::PermitProbe,
            _ => CircuitAction::NoChange,
        };
        Ok(Judgment {
            action,
            confidence: Some(0.95),
        })
    }
}
fn valid_counts(_: &Phase, d: &CircuitData) -> Result<(), Rejection> {
    if d.timeouts > d.completed_requests {
        return Err(Rejection::new(
            "invalid_counts",
            "timeouts exceed completed requests",
        ));
    }
    Ok(())
}
fn valid_phase(p: &Phase, d: &CircuitData) -> Result<(), Rejection> {
    let valid = match p {
        Phase::Closed => d.cooldown_until.is_none() && d.active_probe.is_none(),
        Phase::Open => d.cooldown_until.is_some() && d.active_probe.is_none(),
        Phase::HalfOpen => d.cooldown_until.is_none() && d.active_probe.is_some(),
    };
    if valid {
        Ok(())
    } else {
        Err(Rejection::new(
            "invalid_phase",
            "phase and runtime data disagree",
        ))
    }
}
fn sustained_distress<I>(d: &CircuitData, _: &I, _: Instant) -> Result<(), Rejection> {
    if d.completed_requests >= 20 && d.timeouts as f64 / d.completed_requests as f64 >= 0.5 {
        Ok(())
    } else {
        Err(Rejection::new(
            "insufficient_distress",
            "opening threshold not met",
        ))
    }
}
fn start_cooldown<I>(d: &mut CircuitData, _: &I, now: Instant) -> Result<(), Rejection> {
    // Short duration keeps the offline example quick; production policy is app-owned.
    d.cooldown_until = Some(
        now.checked_add(Duration::from_millis(5))
            .ok_or_else(|| Rejection::new("time_overflow", "cannot set deadline"))?,
    );
    d.active_probe = None;
    Ok(())
}
fn cooldown_elapsed<I>(d: &CircuitData, _: &I, now: Instant) -> Result<(), Rejection> {
    if d.cooldown_until.is_some_and(|deadline| now >= deadline) {
        Ok(())
    } else {
        Err(Rejection::new(
            "cooldown",
            "recovery cooldown has not elapsed",
        ))
    }
}
fn reserve_probe<I>(d: &mut CircuitData, _: &I, _: Instant) -> Result<ReservedProbe, Rejection> {
    let probe_id = d.next_probe_id;
    d.next_probe_id = probe_id
        .checked_add(1)
        .ok_or_else(|| Rejection::new("probe_ids_exhausted", "cannot reserve probe"))?;
    d.cooldown_until = None;
    d.active_probe = Some(probe_id);
    Ok(ReservedProbe { probe_id })
}
fn active_probe(d: &CircuitData, event: &ProbeEvent, _: Instant) -> Result<(), Rejection> {
    let probe_id = match event {
        ProbeEvent::Succeeded { probe_id } | ProbeEvent::Failed { probe_id } => *probe_id,
    };
    if d.active_probe == Some(probe_id) {
        Ok(())
    } else {
        Err(Rejection::new(
            "stale_probe",
            "completion does not match the reserved probe",
        ))
    }
}
fn clear_health(d: &mut CircuitData, _: &ProbeEvent, _: Instant) -> Result<(), Rejection> {
    d.completed_requests = 0;
    d.timeouts = 0;
    d.cooldown_until = None;
    d.active_probe = None;
    Ok(())
}
async fn run_reserved_probe(probe: ReservedProbe) -> ProbeEvent {
    // Replace this bounded simulation with application I/O and map expected errors to Failed.
    let succeeded = tokio::time::timeout(Duration::from_secs(1), async {
        tokio::task::yield_now().await;
        true
    })
    .await
    .unwrap_or(false);
    println!(
        "Post-commit probe {} completed: {succeeded}",
        probe.probe_id
    );
    if succeeded {
        ProbeEvent::Succeeded {
            probe_id: probe.probe_id,
        }
    } else {
        ProbeEvent::Failed {
            probe_id: probe.probe_id,
        }
    }
}
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let definition = state_machine! {
        phase: Phase, data: CircuitData, action: CircuitAction, event: ProbeEvent,
        invariants: [valid_counts, valid_phase], no_change: CircuitAction::NoChange,
        transitions: [
            Phase::Closed + action(CircuitAction::Open) => Phase::Open {
                min_confidence: 0.85, guard: sustained_distress, update: start_cooldown,
            },
            Phase::Open + action(CircuitAction::PermitProbe) => Phase::HalfOpen {
                guard: cooldown_elapsed, update: reserve_probe, effect: run_reserved_probe,
            },
            Phase::HalfOpen + event(ProbeEvent::Succeeded { .. }) => Phase::Closed { guard: active_probe, update: clear_health },
            Phase::HalfOpen + event(ProbeEvent::Failed { .. }) => Phase::Open { guard: active_probe, update: start_cooldown },
            Phase::Closed + evaluation_error(_) => Phase::Open { guard: sustained_distress, update: start_cooldown },
            Phase::Open + evaluation_error(_) => Phase::HalfOpen { guard: cooldown_elapsed, update: reserve_probe, effect: run_reserved_probe },
            Phase::HalfOpen + evaluation_error(_) => unchanged {},
        ],
    };
    let executor = StateMachineExecutor::builder(definition)
        .store(InMemory::new(
            Phase::Closed,
            CircuitData {
                cooldown_until: None,
                active_probe: None,
                next_probe_id: 1,
                completed_requests: 100,
                timeouts: 75,
            },
        ))
        .build()?;
    let controller = Controller::builder()
        .judge(DemoJudge)
        .inference_timeout(Duration::from_millis(500))
        .build()?;
    for _ in 0..3 {
        let (phase, data) = executor.state()?;
        let state = CircuitState {
            phase,
            can_probe: data.cooldown_until.is_some_and(|t| Instant::now() >= t),
            completed_requests: data.completed_requests,
            timeouts: data.timeouts,
        };
        let outcome = executor.execute(controller.evaluate(&state).await).await?;
        println!("{outcome:#?}");
        tokio::time::sleep(Duration::from_millis(6)).await;
    }
    assert_eq!(executor.state()?.0, Phase::Closed);
    Ok(())
}
