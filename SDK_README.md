# Reflex Rust SDK

Reflex turns typed model recommendations into guarded state transitions. The application supplies state to a controller; a declarative executor checks current state, guards, confidence policy, and invariants before committing. Declared async effects run after commit and return events to the same machine.

This workspace contains the initial implementation. It uses Rust 1.92+ and Tokio. Packages are local and marked `publish = false` while the API and release metadata are being developed.

| Crate | Purpose |
| --- | --- |
| `reflex` | Judge/controller interfaces, proposals, execution outcomes, in-memory state machines, and effect supervision |
| `reflex-macros` | `state_machine!`, re-exported by `reflex` |
| `typesafe-ai` | Standalone HTTP client, `questions!`, and typed Choice, Score, and Noul answers |
| `reflex-typesafe` | Inject an instantiated TypeSafe client and task as a judge |

## Run the circuit breaker

From the workspace root:

```sh
cargo run -p reflex --example circuit_breaker
```

The example needs no credentials or network access. It demonstrates Closed → Open → HalfOpen → Closed, a confidence threshold, cooldown and probe guards, invariants, evaluation-error transitions, an async recovery probe, and a no-change action. Its deterministic judge makes the execution behavior reproducible.

The complete source is [circuit_breaker.rs](crates/reflex/examples/circuit_breaker.rs). The [conceptual SDK guide](REFLEX_SDK.md) explains the same API incrementally.

## Use the local crates

For an application alongside this workspace, adjust these paths to its location:

```toml
[dependencies]
reflex = { path = "../reflex/crates/reflex" }
reflex-typesafe = { path = "../reflex/crates/reflex-typesafe" }
typesafe-ai = { path = "../reflex/crates/typesafe-ai" }
serde = { version = "1", features = ["derive"] }
tokio = { version = "1", features = ["rt-multi-thread", "macros"] }
```

A custom judge implements `Judge<S, A>` with an async `judge(&self, &S)` returning `Result<Judgment<A>, JudgeError>`. The core has no provider dependency. Construct a controller with `Controller::builder().judge(judge).inference_timeout(timeout).build()?` and pass its whole evaluation result to the executor:

```rust
let evaluation = controller.evaluate(&state).await;
let outcome = executor.execute(evaluation).await?;
```

An action is a recommendation; it cannot mutate state directly. A deliberate no-op is an application action accepted by `no_change`, or an explicit `=> unchanged {}` row.

## Define a machine

`state_machine!` takes `phase`, `data`, `action`, and `event` types, optional `invariants` and `no_change`, and a `transitions` list. Use `event: ()` when there are no application events. Phase values are enum paths; input patterns support ordinary Rust matching. For example:

```rust
Phase::Open + action(CircuitAction::PermitProbe) => Phase::HalfOpen {
    min_confidence: 0.85,
    guard: cooldown_elapsed,
    update: reserve_probe,
    effect: run_reserved_probe,
}
```

Hooks have these signatures, where `I` is the row's action, evaluation error, or event type:

```rust
fn invariant(phase: &Phase, data: &Data) -> Result<(), Rejection>;
fn guard(data: &Data, input: &I, now: Instant) -> Result<(), Rejection>;
fn update(data: &mut Data, input: &I, now: Instant) -> Result<Payload, Rejection>;
async fn effect(payload: Payload) -> Event;
```

Without an effect, an update returns `Result<(), Rejection>`. Function items and owned `Send + Sync + 'static` closures are supported. An effect closure can capture an `Arc` or reusable application client and clone it into each async invocation. The macro type-checks its payload against the update's result.

An unchanged row may have a guard and action-confidence threshold, but cannot have an update or effect. `min_confidence` on error or event rows is a compile error. Duplicate matching rows, including overlap with `no_change`, reject before any guard runs.

Construct the executor with `StateMachineExecutor::builder(definition).store(InMemory::new(phase, data)).build()?`. Construction validates thresholds and all initial invariants. `executor.state()?` returns a consistent cloned `(phase, data)` for preparing model input. Submit application facts with `executor.handle_event(event).await?`; self-transitions can update counters.

## Execution and effect outcomes

Current invariants, unique matching, confidence, guards, candidate updates, candidate invariants, and commit run under one mutex. A failed check discards the candidate and any prepared effect. Effects execute outside the lock, after the phase and data are installed together. Every returned event goes through the same checks.

`ExecutionOutcome::Applied(receipt)` describes the initiating transition. `receipt.effects` records each effect completion and its transition outcome in execution order. It also exposes `Panicked`, `TimedOut`, `CompletionFailed`, and `ChainLimitReached`. A completed effect whose event is rejected remains visible as `EffectOutcome::Completed(ExecutionOutcome::Rejected { .. })`.

`ExecutionOutcome::Rejected { reason, evaluation_error }` confirms no initiating transition occurred. Both rejected outcomes and applied receipts retain the triggering `EvaluationError`, when present. Confidence-policy failures are execution rejections and do not invoke evaluation-error rows.

Calls wait for the effect chain. Once a call is polled, a Tokio supervisor owns the work: cancelling the awaiting caller does not cancel an already submitted operation. `recent_reports()` exposes completed results, including those whose caller was cancelled. Reports have process-local execution identifiers and are retained in completion order.

| Builder setting | Default |
| --- | --- |
| `effect_timeout(Duration)` | 30 seconds per handler |
| `max_effects(usize)` | 16 dispatched effects per input chain |
| `report_capacity(usize)` | 64 completed reports |
| `clock(closure)` | `Instant::now` |

All three limits must be positive. The chain limit can be reached after a completion transition commits: the report then identifies that its next effect was not dispatched. A timed-out handler is dropped and does not generate a completion event. Applications should map expected I/O failures and shorter timeouts into their own events; supervise the remaining failure outcomes and recover explicitly.

## Telemetry

Reflex records two outcome counters, execution spans, and structured error/rejection logs. See [Reflex telemetry](crates/reflex/TELEMETRY.md) for the contract and optional `.name(...)` / `.meter(...)` configuration.

## TypeSafe integration

The client includes request metrics, traces, and structured logs. See [TypeSafe telemetry](crates/typesafe-ai/TELEMETRY.md) for the instrumentation contract and a direct-to-Datadog example.

Use `TypeSafeClient::builder().api_key(key).timeout(duration).build()?`, then define a `SystemOneTask` using `questions!`. Choice values are typed application values; serializing a value to a string uses that string as its wire label, while other values use JSON text. Duplicate labels fail task construction. Score and Noul retain their own answer types.

Inject the client and task with `TypeSafeJudge::new(client, task).select_answer(|answers| answers.action)`. The adapter forwards that answer's action and optional confidence. `last_response()` exposes the latest successful response's model, usage, request ID, and distribution. Concurrent requests share this latest-response slot; it is not a per-decision audit trail.

The standalone client uses the [TypeSafe HTTP contract](https://docs.typesafe.ai/api), validates answers and distributions, disables redirects, and retries selected transient HTTP statuses with bounded backoff. Its deadline includes retries; the controller independently bounds the whole judge operation. The client defaults to two retries and a ten-second deadline. The adapter preserves status-specific failure codes such as `typesafe_http_401` for error guards.

A live evaluation example is [jev.rs](crates/reflex-typesafe/examples/jev.rs). It uses the environment's `TYPESAFE_API_KEY` when explicitly run:

```sh
cargo run -p reflex-typesafe --example jev
```

This performs a real provider request. Development tests use loopback HTTP servers and never require a key.

## Guarantees and current scope

The first storage implementation is authoritative in-memory state. Database transactions, distributed commits, durable effect delivery, persistent audit logs, and exactly-once I/O are not implemented. Tokio runtime shutdown or process failure can interrupt work after commit. Async timeouts require cooperative yielding; synchronous blocking work is not preempted.

Runtime data must clone into an isolated candidate. Hooks must not mutate shared objects or perform external I/O. Phase/data cloning and destruction must be side-effect-free and must not panic. The library catches ordinary synchronous-hook and effect panics when Rust uses unwinding; aborting panics terminate the process. A verified transition preserves the constraints actually declared and implemented by the application.

Proposals are checked against current machine state, but are not automatically tied to a snapshot revision. Applications needing stronger freshness checks should carry identifiers or revisions in their actions and check them in guards. Explicit abstention, a generic score policy, external storage traits, and state hierarchies remain future design work.

## Validate and browse the API

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --all-targets --locked
cargo test --workspace --doc --locked
cargo doc --workspace --no-deps --locked
```

Tests cover confidence validation, error routing, initial/current/candidate invariants, rollback, concurrent reservations, cancelled callers, effect timeouts and panics, rejected completion events, bounded effect chains, macro compile errors, HTTP requests/retries/validation, and the full provider-to-executor path.
