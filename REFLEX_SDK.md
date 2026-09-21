# Reflex SDK design

**System 1 for software systems.**

Reflex is a Rust library for model-recommended, guarded system actions. The application supplies typed state, the model recommends an action, and a declarative executor checks and applies a permitted transition.

The initial Rust implementation is available in `crates/`. See the [SDK quickstart](SDK_README.md) for setup, runtime defaults, and current limits, and the [compiled circuit-breaker example](crates/reflex/examples/circuit_breaker.rs) for a complete application. The snippets below explain individual concepts.

## 1. Core concepts

### 1.1 The decision and execution loop

```text
typed system state
        |
        v
Jev heuristic judgment
  choice + score + confidence + abstention
        |
        v
deterministic guard
  legality + policy + resource bounds
        |
        v
verified state transition
```

The executor enforces the deterministic guard through declared transitions, guard functions, invariants, and protected commits. Action confidence is carried with the proposed decision. Domain-specific scores and abstention remain concepts whose executor APIs are still to be specified.

A **verified state transition** is a committed change that preserves the application's specified correctness constraints. The guarantee depends on the declared rules, custom functions, and storage enforcing those constraints. It does not imply that the model chose the best action.

### 1.2 State supplied to the model

The application gathers data from memory, a database, or an API and constructs a serializable Rust value. That value is the input to `controller.evaluate(&state)`.

Model input can be a small view of the system, such as recent request counts. The executor separately reads current operational state before applying a result. Preparing model input does not lock the system for the duration of inference.

### 1.3 Controller and actions

The application defines a serializable action type. A circuit breaker might use `Open`, `PermitProbe`, and `NoChange`. Choosing `NoChange` is a successful model decision to leave the system unchanged.

The controller uses a configured judge to obtain an action and returns:

```rust
Result<ProposedDecision<A>, EvaluationError>
```

`ProposedDecision<A>` exposes the recommendation through `action()` and its optional confidence through `confidence() -> Option<f64>`. `EvaluationError` describes a failure to obtain a valid decision, such as a timeout, provider error, or invalid response. The inference deadline covers the whole judgment operation, including SDK retries.

The application passes the complete result to the executor:

```rust
let evaluation = controller.evaluate(&state).await;
let outcome = executor.execute(evaluation).await?;
```

There is no `?` after `evaluate`: both a proposed decision and an evaluation failure are inputs the executor can handle. Provider setup and the judge interface are described in [TypeSafe SDK implementation notes](TYPESAFE_SDK.md).

#### Confidence and domain scores

The judge supplies `Judgment<A> { action: A, confidence: Option<f64> }`. The controller preserves both fields in the proposed decision. A confidence value must be finite and within `0.0..=1.0`; the controller rejects an invalid value as an `EvaluationError` before creating a proposal. Missing confidence remains `None` and is not an evaluation failure.

Confidence and a domain score answer different questions. Confidence describes certainty about an answer; a domain score measures something on an application-defined scale, such as distress severity. TypeSafe derives confidence from the answer's probability distribution. It is not a guarantee of correctness or a calibrated probability that executing an action will succeed. See [TypeSafe confidence](https://docs.typesafe.ai/confidence) and [question primitives](https://docs.typesafe.ai/primitives).

Applications declare a minimum confidence on individual action transitions:

```rust
Phase::Closed + action(CircuitAction::Open) => Phase::Open {
    min_confidence: 0.85,
    guard: sustained_distress,
    update: start_cooldown,
}
```

The threshold is illustrative application policy, not a library default. It must also be finite and within `0.0..=1.0`. The library rejects invalid threshold configuration at definition or executor construction time.

| Confidence policy | Execution behavior |
| --- | --- |
| Present and at least the declared minimum | Continue to the guard and invariant checks |
| Below the minimum | Reject with `insufficient_confidence` |
| Missing when a minimum is required | Reject with `missing_confidence` |
| No minimum declared | Apply the ordinary guards and invariants without a confidence threshold |

Low or missing required confidence produces `ExecutionOutcome::Rejected` without mutation or effects. It does not become an `EvaluationError` or trigger an `evaluation_error(...)` rule. The application receives the rejection; automatic alternative behavior is not part of this policy.

`min_confidence` applies only to action rows, including explicit unchanged action rows. Error and event rows have no model confidence, so declaring a threshold on them is invalid. The global `no_change` shorthand has no confidence threshold.

The TypeSafe adapter retains the full answer distribution for diagnostics. Domain scores, if introduced later, should have explicit names and scales and retain their own answer confidence; the initial core API has no generic `score` field. Explicit abstention is also a separate design question and is not inferred automatically from a low-confidence rejection.

### 1.4 A declarative state-machine executor

The library provides `StateMachineExecutor`. Applications define a phase enum, runtime data, and a table of legal transitions. Each transition names a source phase, an input, a target phase, and optional guard, update, and effect functions.

| Component | Purpose |
| --- | --- |
| Phase | Identify the operating mode, such as `Closed` or `Open` |
| Runtime data | Hold values such as counters, deadlines, and probe identifiers |
| Transition | Declare which input can move one phase to another |
| Guard | Check whether a declared transition is currently permitted |
| Update | Modify candidate data and prepare owned input for a declared effect |
| Effect | Run application-supplied async work after commit and return an event |
| Invariant | Define a condition that every valid phase/data pair must satisfy |

A guard asks whether a particular transition may happen now. An invariant asks whether the system state is valid, regardless of the input. Applications declare named invariant functions in `invariants: [...]`; each receives `&Phase` and `&Data` and returns `Result<(), Rejection>`. Every declared invariant must pass.

The executor accepts three kinds of input:

| Input | Example | Entry point |
| --- | --- | --- |
| Model-proposed action | Open the circuit | `execute(Ok(decision))` |
| Evaluation failure | Model request timed out | `execute(Err(error))` |
| Application event | Recovery probe succeeded | `handle_event(event)` |

Evaluation failure is an input, not inherently an operating phase. The circuit can remain closed when inference times out. A failure rule declares what should happen in that phase. An application can define a persistent degraded mode if its policy requires one.

Failure handling belongs in the transition table through `evaluation_error(...)` rules. These rules can inspect the error, check current conditions, change state, or explicitly leave it unchanged. An unmatched failure is rejected with the original error retained for the caller; it is not silently treated as a successful no-op.

### 1.5 Storage and safe execution

`.store(...)` specifies where authoritative machine state lives and how the executor accesses and commits it:

```rust
.store(InMemory::new(Phase::Closed, initial_data))
```

`InMemory` holds phase and data under a lock. A database adapter would need equivalent transactional or conditional-update guarantees. Updating a detached local copy does not verify a change to an external system. The public storage interface remains provisional.

For each input, the library performs one protected operation:

1. Read current phase and data, check all invariants, and capture execution time.
2. Find the unique matching rule, or handle a declared no-change action.
3. Check any declared minimum confidence, then the rule's guard.
4. Run its update against an isolated candidate and set the target phase.
5. Check all invariants against the completed candidate phase and data.
6. Commit, release protected state access, and make any declared effect eligible for library dispatch. Record the transition receipt separately from effect completion.

All invariants are also checked when initializing the machine; a violation fails construction. During execution, an invariant violation in either current state or the candidate rejects the input without mutation or effects. Candidate invariants run after the update and target-phase assignment, not between individual assignments inside the update. These are runtime checks; their guarantee depends on every mutation path preserving the constraints.

Undefined or ambiguous transitions are rejected. Guards do not choose between overlapping rules: matching happens first, and a failed confidence check or guard rejects that input. Action, evaluation-error, and event rules all use the same execution sequence. A failure rule must declare its own required guards; it does not inherit checks from an action rule.

Guards, updates, and invariants are short synchronous functions. Updates must not perform external I/O or mutate shared objects. Candidate data must be isolated; `Clone` alone does not guarantee isolation when data contains shared mutable handles. Other writers must preserve the same constraints.

An explicit unchanged rule checks all invariants on current state, any action-confidence threshold, and any guard, then returns a receipt with `changed: false` and no effects. It has no update or effect hook and performs no mutation. A normal transition can stay in the same phase while updating its data.

### 1.6 Outcomes and effects

```rust
pub enum ExecutionOutcome<R> {
    Applied(R),
    Rejected { reason: Rejection, evaluation_error: Option<EvaluationError> },
}

pub struct Rejection {
    pub code: String,
    pub message: String,
}

pub struct TransitionReceipt<P> {
    pub from: P,
    pub to: P,
    pub changed: bool,
    pub evaluation_error: Option<EvaluationError>,
    pub effects: Vec<EffectOutcome<P>>,
}
```

`Applied` confirms a committed transition or an explicit unchanged result. `Rejected` confirms that the executor declined the input without changing state. An executor error indicates an operational problem, such as a storage failure; an external commit outcome may be uncertain.

An effect is an application-supplied async function declared on a transition. The update prepares an owned input, such as a reserved probe identifier. The library invokes the effect only after the candidate passes every invariant, the commit succeeds, and protected state access is released. A failed or uncertain commit does not dispatch the effect.

The effect receives that owned input, not mutable live machine state. Its returned event is fed back into the state machine by the library and passes normal transition checks. Applications supply the I/O implementation without writing a dispatch loop.

`Applied(receipt)` describes the initiating transition; it does not assert that the effect or its completion transition succeeded. A failed effect cannot roll back the committed state. Expected failures and timeouts should be mapped to application events by the handler. Handler errors, panics, cancellation, and rejected completion events require explicit runtime reporting and recovery policy; they must not be reported as rejection of an already committed transition or silently discarded.

The driving call waits for a supervised Tokio task to complete its bounded effect chain. Cancelling the awaiting caller does not cancel submitted work while the runtime remains alive. `receipt.effects` reports effect and completion outcomes; `executor.recent_reports()` retains completed call results, including cancelled callers. Defaults are a 30-second effect timeout, 16 effects per chain, and 64 retained reports. These settings are configurable. Delivery is process-local, with no automatic effect retries or exactly-once guarantee.

The original evaluation error is retained in `receipt.evaluation_error` or the rejected outcome's `evaluation_error`. `EffectOutcome` distinguishes completed events (including rejection), completion-engine errors, handler panics, timeouts, and an exhausted effect-chain limit.

### 1.7 Executor interface

`StateMachineExecutor` implements the executor interface. Applications with other execution models can implement it themselves:

```rust
pub trait Executor {
    type Action;
    type Receipt;
    type Error;

    async fn execute(
        &self,
        evaluation: Result<ProposedDecision<Self::Action>, EvaluationError>,
    ) -> Result<ExecutionOutcome<Self::Receipt>, Self::Error>;
}
```

Its action type must match the judge's action type. `handle_event` is an additional method on the state-machine executor.

## 2. Example: a circuit breaker with recovery

The circuit admits traffic while closed, blocks traffic while open, and reserves one recovery probe while half-open. Jev recommends opening, probing, or leaving it unchanged. The declared probe effect returns its result as an event, which the library applies through the state machine.

### 2.1 Define the types

```rust
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};
use reflex::{
    state_machine, EvaluationError, ExecutionOutcome, Executor,
    InMemory, Rejection, StateMachineExecutor,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Phase {
    Closed,
    Open,
    HalfOpen,
}

// Input prepared by the application for Jev.
#[derive(Serialize)]
struct CircuitState {
    phase: Phase,
    can_probe: bool,
    completed_requests: u64,
    timeouts: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum CircuitAction {
    Open,
    PermitProbe,
    NoChange,
}

// Authoritative operational data used by the executor.
#[derive(Clone)]
struct CircuitData {
    cooldown_until: Option<Instant>,
    active_probe: Option<u64>,
    next_probe_id: u64,
    completed_requests: u64,
    timeouts: u64,
}

enum ProbeEvent {
    Succeeded { probe_id: u64 },
    Failed { probe_id: u64 },
}

struct ReservedProbe {
    probe_id: u64,
}
```

The model can request a probe but cannot report its outcome. The application's `can_probe` field summarizes cooldown eligibility for the model; the executor checks the actual deadline again before reserving a probe.

### 2.2 Declare transitions, including evaluation failures

Declare the machine with `state_machine!`:

```rust
let definition = state_machine! {
    phase: Phase,
    data: CircuitData,
    action: CircuitAction,
    event: ProbeEvent,

    invariants: [
        valid_request_counts,
        phase_matches_runtime_data,
    ],
    no_change: CircuitAction::NoChange,

    transitions: [
        Phase::Closed + action(CircuitAction::Open) => Phase::Open {
            min_confidence: 0.85,
            guard: sustained_distress,
            update: start_cooldown,
        },
        Phase::Open + action(CircuitAction::PermitProbe) => Phase::HalfOpen {
            guard: cooldown_elapsed,
            update: reserve_probe,
            effect: run_reserved_probe,
        },
        Phase::HalfOpen + event(ProbeEvent::Succeeded { .. }) => Phase::Closed {
            guard: active_probe,
            update: clear_health,
        },
        Phase::HalfOpen + event(ProbeEvent::Failed { .. }) => Phase::Open {
            guard: active_probe,
            update: start_cooldown,
        },

        Phase::Closed + evaluation_error(_) => Phase::Open {
            guard: sustained_distress,
            update: start_cooldown,
        },
        Phase::Open + evaluation_error(_) => Phase::HalfOpen {
            guard: cooldown_elapsed,
            update: reserve_probe,
            effect: run_reserved_probe,
        },
        Phase::HalfOpen + evaluation_error(_) => unchanged {},
    ],
};
```

A model proposal to open requires confidence of at least 0.85 and sustained distress. If evaluation fails while closed, the deterministic error rule requires sustained distress without a model-confidence check. If it fails while open, recovery requires an elapsed cooldown. A failed guard returns a rejection and leaves state unchanged. While half-open, an evaluation failure explicitly leaves the active probe alone.

`evaluation_error(_)` matches any evaluation failure. A rule may use a narrower error pattern or inspect the error in its guard. The core categories are `Timeout`, `Judge(JudgeError)`, and `InvalidConfidence`. Judge errors expose a code and message.

`no_change` accepts the model's `NoChange` action in any valid phase. `=> unchanged {}` explicitly handles the failure input without mutation. Both return `Applied` with `changed: false`, but the input and diagnostic record distinguish them.

### 2.3 Implement application rules

Transition guards and updates receive current data or candidate data, the matched input, and execution time. The input is an action, evaluation error, or application event, depending on the row. Hooks that ignore it can be generic and shared across rows:

```rust
fn reject(code: &str, message: &str) -> Rejection {
    Rejection { code: code.into(), message: message.into() }
}

fn valid_request_counts(
    _phase: &Phase,
    data: &CircuitData,
) -> Result<(), Rejection> {
    if data.timeouts > data.completed_requests {
        return Err(reject(
            "invalid_counts",
            "Timeouts cannot exceed completed requests",
        ));
    }
    Ok(())
}

fn phase_matches_runtime_data(
    phase: &Phase,
    data: &CircuitData,
) -> Result<(), Rejection> {
    let valid = match phase {
        Phase::Closed => {
            data.cooldown_until.is_none() && data.active_probe.is_none()
        }
        Phase::Open => {
            data.cooldown_until.is_some() && data.active_probe.is_none()
        }
        Phase::HalfOpen => {
            data.cooldown_until.is_none() && data.active_probe.is_some()
        }
    };

    if valid {
        Ok(())
    } else {
        Err(reject(
            "invalid_phase_data",
            "Runtime data is inconsistent with the circuit phase",
        ))
    }
}

fn cooldown_elapsed<I>(
    data: &CircuitData,
    _input: &I,
    now: Instant,
) -> Result<(), Rejection> {
    match data.cooldown_until {
        Some(deadline) if now >= deadline => Ok(()),
        _ => Err(reject("cooldown", "Recovery cooldown has not elapsed")),
    }
}

fn sustained_distress<I>(
    data: &CircuitData,
    _input: &I,
    _now: Instant,
) -> Result<(), Rejection> {
    if data.completed_requests >= 20
        && data.timeouts as f64 / data.completed_requests as f64 >= 0.5
    {
        Ok(())
    } else {
        Err(reject("insufficient_distress", "Opening threshold not met"))
    }
}

fn reserve_probe<I>(
    data: &mut CircuitData,
    _input: &I,
    _now: Instant,
) -> Result<ReservedProbe, Rejection> {
    let probe_id = data.next_probe_id;
    data.next_probe_id = probe_id.checked_add(1)
        .ok_or_else(|| reject("probe_ids_exhausted", "Cannot allocate probe"))?;
    data.cooldown_until = None;
    data.active_probe = Some(probe_id);
    Ok(ReservedProbe { probe_id })
}
```

`reserve_probe` returns the owned input for `run_reserved_probe`. The macro checks that an update's success type matches its declared effect's input type. Rows without effects return `Result<(), Rejection>` from their updates.

The async handler is also application code:

```rust
async fn run_reserved_probe(probe: ReservedProbe) -> ProbeEvent {
    // Application helper: true on success, false on failure or timeout.
    let succeeded = run_recovery_probe().await;

    if succeeded {
        ProbeEvent::Succeeded { probe_id: probe.probe_id }
    } else {
        ProbeEvent::Failed { probe_id: probe.probe_id }
    }
}
```

`run_recovery_probe` must bound the network request and map failure or timeout to `false`. The runtime invokes this handler after commit and submits its returned event; the handler never commits machine state itself.

The remaining synchronous hook bodies are omitted:

| Hook | Required behavior |
| --- | --- |
| `start_cooldown<I>` | Update candidate data with a checked five-second deadline, clear the active probe, and return `Ok(())` |
| `active_probe` | Accept a `ProbeEvent` only if its identifier matches the current active probe |
| `clear_health` | Clear counters, deadline, and active probe; return `Ok(())` |

The library sets the phase declared by the row and checks every invariant against that phase and the updated data. No application hook commits the candidate itself.

### 2.4 Construct the executor

```rust
let executor = StateMachineExecutor::builder(definition)
    .store(InMemory::new(Phase::Closed, CircuitData {
        cooldown_until: None,
        active_probe: None,
        next_probe_id: 1,
        completed_requests: 100,
        timeouts: 35,
    }))
    .build()?;
```

The builder checks every invariant against initial state. Failure handling is already part of the definition. Storage protects concurrent transitions so only one probe reservation can succeed.

### 2.5 Configure the model

```rust
use typesafe_ai::{choice, questions, SystemOneTask, TypeSafeClient};
use reflex::Controller;
use reflex_typesafe::TypeSafeJudge;

let client = TypeSafeClient::builder()
    .api_key(std::env::var("TYPESAFE_API_KEY")?)
    .timeout(Duration::from_secs(2))
    .build()?;

let task = SystemOneTask::builder()
    .model("jev-1.13.0")
    .questions(questions! {
        action: choice(
            "Recommend a circuit action or no action. When closed, consider \
             opening based on timeout evidence. When open and can_probe \
             is true, consider permitting a recovery probe. Otherwise \
             recommend no action. While half-open, await the probe result. \
             Old timeout counts do not prove continued failure while open.",
            [
                (
                    CircuitAction::Open,
                    "Open the circuit",
                ),
                (
                    CircuitAction::PermitProbe,
                    "Permit one recovery probe",
                ),
                (CircuitAction::NoChange, "Leave the circuit unchanged"),
            ],
        ),
    })
    .build()?;

let judge = TypeSafeJudge::new(client, task)
    .select_answer(|answers| answers.action);

let controller = Controller::builder()
    .judge(judge)
    .inference_timeout(Duration::from_millis(500))
    .build()?;

```

The standalone client and task produce the judge injected into the controller. They do not own the executor or its state.

### 2.6 Evaluate and let the library coordinate effects

```rust
// Application helper: read current operational state and prepare CircuitState.
let state = read_circuit_state().await?;
let evaluation = controller.evaluate(&state).await;

match executor.execute(evaluation).await? {
    ExecutionOutcome::Applied(receipt) => {
        println!("Transition committed: {:?} -> {:?}", receipt.from, receipt.to);
    }
    ExecutionOutcome::Rejected { reason, .. } => {
        println!("Execution rejected: {}", reason.message);
    }
}
```

`read_circuit_state` is an application helper that can call `executor.state()?` to obtain a consistent cloned `(phase, data)` and construct the smaller model view. Protected access is released before inference.

For a permitted probe, the library coordinates this sequence:

```text
Check cooldown and prepare ReservedProbe
              ↓
Check invariants and commit HalfOpen
              ↓
Release protected state access
              ↓
Invoke run_reserved_probe with the committed reservation
              ↓
Submit its returned ProbeEvent to the state machine
              ↓
Check and commit Closed or Open
```

The application supplies the handler in the definition; it does not iterate over receipt effects or manually forward the handler's event. It may still call `handle_event` for independent application events.

The receipt describes the initial transition even if effect execution has since progressed. Inspect `receipt.effects` for effect and completion-transition outcomes; the call waits for the bounded chain. A probe reservation blocks a second probe; stale or duplicate completion events are rejected by transition lookup or the probe-identifier guard.

Traffic admission and metrics collection remain application responsibilities. They must respect current phase and update counters safely. The application repeats evaluation with fresh model input as needed; each execution checks current operational state again.

## 3. Remaining design questions

- Public storage and read interfaces, including external transaction support.
- Evaluation-error categories and how outcomes retain original errors and input metadata.
- Domain-specific judgment scores and explicit abstention semantics and API representation.
- Proposed-decision identifiers, source versions, and async trait bounds.
- Final macro syntax, clock configuration, and optional event types.
- Effect waiting versus supervised background dispatch, outcome reporting, handler errors, timeouts, cancellation, and bounded event chains.
- External idempotency, durable effect delivery, and uncertain commit outcomes.

## References

- [Declarative executor specification](DECLARATIVE_EXECUTOR.md)
- [TypeSafe SDK implementation notes](TYPESAFE_SDK.md)
