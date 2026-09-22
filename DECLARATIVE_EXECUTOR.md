# Declarative state-machine executor

Execution details for the declarative executor in [Reflex](REFLEX_SDK.md). The macro and in-memory runtime are implemented in the Rust workspace. See the [SDK quickstart](SDK_README.md) for runnable examples and exact supervision defaults. External storage remains a design direction.

## 1. Purpose

`StateMachineExecutor` implements the executor protocol from the SDK. Applications declare states and legal transitions, then supply the small functions that contain their policy and update logic.

```rust
let evaluation = controller.evaluate(&state).await;
let outcome = executor.execute(evaluation).await?;
```

The controller still receives application-prepared model input and returns `Result<ProposedDecision<A>, EvaluationError>`. The executor treats model-proposed actions and evaluation failures as distinct inputs to the declared transition table.

| Library responsibility | Application responsibility |
| --- | --- |
| Find the declared transition for the current phase and input | Define phases, actions, events, and runtime data |
| Reject undefined or ambiguous transitions | Specify legal source/input/target combinations |
| Invoke guards and invariants in a fixed order | Implement policy predicates and named state invariants |
| Prepare an isolated candidate and set its target phase | Update application data on that candidate |
| Coordinate storage access and commit | Select a storage implementation with suitable guarantees |
| Dispatch declared effects after commit and feed returned events into the machine | Supply async handlers and their owned input types |
| Keep transition, effect, and completion outcomes distinct | Define recovery policy for external work |
| Match evaluation failures against declared rules | Define failure transitions, guards, or explicit unchanged outcomes |

The library provides the `Executor` implementation for a conventional state machine. The existing trait remains available for other execution models.

## 2. Represent phases and data with ordinary Rust types

A phase identifies where the machine is in its lifecycle. Runtime data holds the values guards and updates use. These are executor-owned types; the model input can remain a smaller, separately prepared value.

```rust
use std::time::Instant;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
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
```

The model selects only `CircuitAction`. Probe events report application facts. Declared effects return them for library submission; independent application events may also be submitted through `handle_event`. This prevents the model's action schema from including an event such as “the probe succeeded.”

The stored machine state consists of a phase plus data. The library sets the phase; update functions mutate only candidate data. Named invariants check relationships between the two. For this example:

- `Closed` has neither a cooldown deadline nor an active probe.
- `Open` has a cooldown deadline and no active probe.
- `HalfOpen` has an active probe and no cooldown deadline.
- Timeouts cannot exceed completed requests.

## 3. Declare the transition table

The `state_machine!` macro describes the execution rules. Each row names a source phase, an input pattern, a target phase, and optional hooks.

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

This declaration determines the graph:

```text
Closed -- Open action, confidence >= 0.85, sustained distress --> Open
Open -- PermitProbe action, after cooldown --> HalfOpen
HalfOpen -- matching successful probe --> Closed
HalfOpen -- matching failed probe --> Open
```

An `Open` action in `HalfOpen`, for example, has no declared transition and is rejected automatically. The application does not write that rejection branch.

`no_change` explicitly permits an action that leaves the machine unchanged in every valid phase. It checks all invariants on current state, emits no effects, and returns a successful receipt with `changed: false`. It does not run transition hooks.

Evaluation failures are inputs, not automatically persistent phases. `evaluation_error(_)` matches any evaluation failure; a narrower pattern or guard can distinguish errors. Core errors are `Timeout`, `Judge(JudgeError)`, and `InvalidConfidence`. An unmatched failure is rejected and retains the original error for the caller.

`=> unchanged {}` explicitly handles an input without mutation or effects. It checks all invariants on current state, runs an optional guard, and returns `changed: false`. An unchanged rule cannot have an update or effect hook. It is different from a self-transition, which may update data while retaining the same phase.

In this example, a failure while closed opens only under sustained distress; a failure while open reserves a probe only after cooldown; a failure while half-open explicitly leaves state unchanged. A failed guard rejects the input. Failure rules declare their own guards and do not inherit them from action rules.

`min_confidence` is an optional library check on an action row. After selecting a unique row and before calling its guard, the executor reads `ProposedDecision::confidence()`. A present value greater than or equal to the threshold passes. A lower value rejects with `insufficient_confidence`; an absent value rejects with `missing_confidence`. No mutation or effects occur on rejection. With no threshold, ordinary guards and invariants apply.

Thresholds must be finite and within `0.0..=1.0`; invalid configuration fails definition or executor construction. Thresholds on error or event rows are invalid because those inputs do not carry model confidence. Explicit unchanged action rows may declare one; the global `no_change` shorthand does not.

The controller preserves valid optional confidence from the judge and rejects non-finite or out-of-range confidence as an `EvaluationError`. Low but valid confidence and absent confidence remain successful proposals; only the matched transition's policy can reject them. These policy rejections do not invoke error rows or automatically select an alternative transition.

The threshold of 0.85 is illustrative, not a default. Confidence does not replace guards or invariants. Domain-specific scores and explicit abstention have no executor API in this initial design.

This first version uses explicit rows and one optional guard, update, and async effect per row. Applications can combine several checks inside a guard function. State hierarchies and entry/exit hooks are outside this proposal.

## 4. Supply synchronous checks and updates, and async effects

Guards, invariants, and updates use synchronous functions or closures. Effects use async functions. The conceptual signatures are:

```rust
// I is CircuitAction, EvaluationError, or ProbeEvent for the matched row.
fn guard(data: &D, input: &I, now: Instant) -> Result<(), Rejection>;

fn update(
    candidate: &mut D,
    input: &I,
    now: Instant,
) -> Result<T, Rejection>;

async fn effect(input: T) -> Event;

fn invariant(phase: &P, data: &D) -> Result<(), Rejection>;
```

Every function named in `invariants: [...]` has the invariant signature above. A guard decides whether a particular transition is allowed; an invariant defines valid state independently of the input. All invariants must pass at initialization, on current state before handling any input, and on the completed candidate before commit.

An initialization violation fails construction. During execution, a violation rejects the input without mutation or effects. Candidate checks run after the update and target-phase assignment; intermediate assignments need not satisfy the invariants. These are runtime checks, and every mutation path must preserve the same constraints.

These are illustrative generic signatures; concrete hooks use the application's types. For a row with an effect, `T` is the owned payload returned by the update and consumed by the effect. The macro type-checks that connection and the returned machine event. A row without an effect uses `T = ()`; omitting its update leaves data unchanged. A row with an effect requires an update that prepares its payload. `now` is an explicit execution-time input supplied by the runtime's clock. It is local to this executor API and allows time-dependent checks and deterministic tests without changing the controller's state-input contract.

For example:

```rust
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

`reject` constructs the SDK's `Rejection { code, message }`. The other application hooks have these responsibilities:

| Hook | Responsibility |
| --- | --- |
| `valid_request_counts` | Require timeouts to be no greater than completed requests |
| `phase_matches_runtime_data` | Enforce the phase/deadline/probe relationships listed above |
| `start_cooldown` | Set a checked future deadline and clear the active probe |
| `active_probe` | Match the event's probe identifier against the active reservation |
| `clear_health` | Clear counters, active probe, and cooldown after successful recovery |
| `sustained_distress<I>` | Ignore the input type and require at least 20 completed requests and a timeout rate of at least 50%; shared by action and error rows |

`start_cooldown` is generic over the input it ignores and can serve action, error, and event rows. It clears the active probe and starts a fresh cooldown, including after a failed probe. A captured configuration or application helper can supply the cooldown duration.

Guards and invariants are read-only. Updates modify an isolated candidate and return owned effect input, or `()` for a row without an effect. They must not perform network calls, mutate shared external objects, or publish work. Rust's signatures cannot enforce that purity: cloning data containing shared mutable handles does not create an isolated candidate.

## 5. Construct the library-provided executor

```rust
let executor = StateMachineExecutor::builder(definition)
    .store(InMemory::new(Phase::Closed, initial_data))
    .build()?;
```

`.store(...)` specifies where authoritative phase/data lives and how it is read and committed. `InMemory` protects it with a lock. External adapters require equivalent transaction or conditional-update guarantees; the public storage API remains provisional.

The builder checks every invariant against initial phase/data. Evaluation failures are handled directly by the declared `evaluation_error(...)` rules. The original error is retained in the applied receipt or rejected outcome as `evaluation_error`.

`StateMachineExecutor` implements the existing `Executor` interface. Its receipt describes the committed transition. Effect dispatch and completion are coordinated by the library and reported separately. A minimal receipt shape is:

```rust
pub struct TransitionReceipt<P> {
    pub from: P,
    pub to: P,
    pub changed: bool,
    pub evaluation_error: Option<EvaluationError>,
    pub effects: Vec<EffectOutcome<P>>,
}
```

The receipt retains the triggering evaluation error and ordered effect outcomes. Completed execution reports have process-local identifiers; durable tracing and correlation remain application responsibilities.

## 6. Execution semantics

For `execute(evaluation)`, the runtime performs one protected operation:

1. Read authoritative phase and data through the store.
2. Check all invariants against current phase/data and capture execution time.
3. Treat a successful decision as an action input, or a failed evaluation as an error input.
4. Handle the declared no-change action, or find the unique matching transition.
5. Check any declared minimum confidence, then invoke the guard. Either rejection ends the operation without a commit or effects.
6. For an explicit unchanged rule, return an unchanged receipt. Otherwise, clone data into an isolated candidate and invoke the update hook.
7. Set the candidate's phase to the declared target.
8. Check all invariants against the completed candidate phase/data.
9. Commit and record a receipt. After protected access is released, make the declared effect and its owned input eligible for library dispatch.

A missing guard accepts the declared edge subject to its confidence threshold, if any, and all invariants. A missing update leaves data unchanged on a row without an effect; all candidate invariants still run. Effect rows require a payload-producing update.

Rule order does not affect behavior. Zero matching rows rejects the input as an invalid transition; an unmatched evaluation failure retains its original error. More than one matching row rejects it as ambiguous before invoking guards or updates. The no-change action must be disjoint from action transition rows; overlapping declarations are invalid. The macro can detect obvious duplicate rows, while the runtime must handle any overlap that cannot be established statically.

Confidence, guard, update, and invariant rejections return `ExecutionOutcome::Rejected`. Successful commits and deliberate no-change results return `Applied(receipt)`. Storage failures return `Executor::Error`; an external backend may report an uncertain commit outcome, which must not be described as confirmed rejection.

Guards, invariants, and updates run synchronously inside the protected operation. The declared effect runs asynchronously after commit, outside that protection. Asynchronous guards, automatic retries of updates, and external I/O inside updates are outside the initial design.

## 7. Declared effects and completion events

Each row may declare one async handler through `effect: run_reserved_probe`. Its update returns an owned `ReservedProbe` payload; the runtime holds that payload until the candidate passes all checks and commits. Only then, outside protected state access, may it invoke the handler.

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

The application implements the bounded network operation. In this example, expected failure or timeout becomes `ProbeEvent::Failed`. The library feeds the returned event into the same guarded transition engine, without invoking Jev. `handle_event(event)` remains available for facts originating elsewhere in the application.

Committing `HalfOpen` and the probe identifier before I/O prevents a second probe reservation. Matching completion events close or reopen the circuit. Stale or duplicate completions reject because no row applies or the identifier fails the `active_probe` guard.

An effect receives owned input from the committed candidate, not mutable live state. A rejected or uncertain commit must not dispatch it. Once committed, external failure cannot roll back the initiating transition. Effect failures and completion rejections must remain observable separately from its successful receipt. The original evaluation error must also remain available when an error row initiates work.

The library runs each submitted operation in a supervised Tokio task and awaits its result. Caller cancellation leaves submitted work running while the runtime remains alive. Effects default to a 30-second timeout and chains to 16 dispatches; these limits are configurable. The receipt reports handler panic, timeout, chain-limit exhaustion, and completion outcomes separately. `recent_reports()` retains the last 64 completed operations by default. Process loss and runtime shutdown can interrupt committed work; the runtime offers no durable delivery, automatic effect retries, or exactly-once I/O.

The application still prepares `state` for `controller.evaluate(&state)`. Machine data is not automatically sent to the model, and callers do not manually iterate over effect payloads.

## 8. State ownership and storage

The state-machine definition does not require every application to move its state into library-owned memory.

- `InMemory` is a reference adapter that protects phase/data with a lock and installs the validated candidate atomically.
- An external store adapter must run validation and the update under an equivalent transaction or conditional commit. It must map the declared phase/data to authoritative system state, not merely update a detached local copy.
- A backend lacking suitable commit guarantees can use the transition evaluator to prepare a proposal, but cannot claim a verified external transition from that evaluation alone.

The exact storage trait is deferred. Its required contract is protected current-state access, isolated candidate preparation, and atomic conditional commit. Version conflicts must be surfaced; hooks must not be retried silently. Timing rules must use a clock meaningful to the authoritative backend. `Instant` is suitable for the in-memory example, not for persisted deadlines shared across machines.

All other mutation paths must preserve the same state constraints. Metrics updates may remain application code or use declared self-transitions as appropriate; there is no new event reducer requirement on the controller.

## 9. Rust implementation approach

Use ordinary Rust enums for phases and actions, typed callbacks for hooks, and a transition-table runtime. The macro is an authoring layer over that runtime.

The macro checks syntax and generates matchers and callback wiring. Rust's type checker verifies phase/input types and hook signatures. Runtime checks establish transition applicability, guard results, and state validity. The macro cannot prove application policy correct or make an external operation transactional.

The implementation separates the transition-table runtime from its macro authoring layer. This keeps execution behavior independent of the declaration syntax.

Existing Rust libraries provide useful precedents:

- [Rust enums and pattern matching](https://doc.rust-lang.org/book/ch06-00-enums.html) provide the language primitives for typed states and events.
- [smlang](https://docs.rs/smlang/latest/smlang/) uses a macro transition table with states, events, guards, and actions.
- [statig](https://docs.rs/statig/latest/statig/) supports typed state machines and optional macros, including state-local data and hierarchical states.

These are design references, not selected dependencies. Reflex's executor additionally needs evaluation-error handling, guarded candidate commits, and effects that are released after commit.

## 10. Initial scope and validation

Start with flat phases, typed model actions, optional application events, explicit no-change declarations, synchronous guards/invariants/updates, optional declared async effects, a clone-isolated data model, and the in-memory store. Keep the current controller and standalone TypeSafe client unchanged.

The essential behavior checks are:

- Undefined and ambiguous transitions reject without mutation or effects.
- Confidence exactly at the minimum passes; lower or missing required confidence rejects before guards or updates.
- Missing confidence is accepted by rules without a threshold; low-confidence rejections do not invoke error rules.
- Invalid thresholds and thresholds on error/event rows fail configuration.
- Controller validation rejects non-finite or out-of-range judgment confidence before creating a proposal.
- Cooldown guards use execution-time state and time.
- Only one probe reservation succeeds under concurrent attempts.
- Invalid initial state fails construction; invariant violations in current state reject before update hooks run.
- Failed updates and candidate invariant checks leave live state unchanged and release no effects.
- Every declared invariant must pass; candidate checks occur after the complete update and phase assignment.
- Successful and failed probe events follow the declared edges; stale identifiers reject.
- Evaluation failures follow declared error rows and pass their guards and every candidate invariant.
- Unmatched failures and failed guards reject while retaining the original evaluation error.
- Explicit unchanged failure rules check every current-state invariant, check any guard, and emit no effects.
- No-change emits no effects and does not run update hooks.
- Failed or uncertain commits never dispatch effects.
- Async handlers run only after commit and outside protected state access.
- Update payload and handler input types agree; completion events pass ordinary guards and invariants.
- Effect failures and rejected completion events remain observable without erasing the initiating receipt.
- Effect dispatch mode and supervision policy must be specified before runtime implementation.

The initial implementation covers these in-memory runtime semantics. External storage traits, durable delivery, stable public API review, and release packaging remain future work.
