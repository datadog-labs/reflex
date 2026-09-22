# Reflex

Reflex is a Rust library for AI-guided state machines, with built-in support for TypeSafe AI’s **Jev**.

You define the state, actions, guards, and invariants. The model recommends an action; Reflex checks it before committing the transition.

```text
Application supplies typed state
              ↓
Judge recommends an action
              ↓
Executor checks legality, guards, and invariants
              ↓
Verified state transition
              ↓
Optional effects perform external work
```

Here, **verified** means that the transition preserves the constraints you declared.

## When to use it

Use Reflex in **control loops** that observe a system, choose an action, and use the resulting feedback to make the next decision.

It fits loops where the choice depends on changing conditions, but execution must obey fixed constraints:

- **Scheduling:** choose which request gets capacity next; enforce resource limits.
- **Congestion control:** adjust the sending rate from latency and loss; enforce rate bounds.
- **Circuit breaking:** open, probe, or close based on service health; enforce cooldowns and probe limits.
- **Recovery:** retry, switch replicas, or rebuild; enforce readiness and recovery budgets.

Your application owns the loop: when it runs, which observations it supplies, and how it measures outcomes. Reflex provides the decision and guarded execution steps.

## How it works

Your application defines the state, actions, and execution rules. Reflex provides the controller and executor that connect them.

| Concept | What it does |
| --- | --- |
| **State** | A Rust value your application prepares for evaluation. It can contain local observations, queried metrics, forecasts, or other relevant evidence. |
| **Judge** | Implements `Judge<S, A>` and returns a typed action with optional confidence. It can call a model or use an ordinary algorithm. |
| **Controller** | Calls the judge with an inference deadline and returns a proposed decision or an evaluation error. It does not change system state. |
| **State machine** | Declares phases and transitions. Guards check whether an action is allowed; invariants check properties that every committed state must satisfy. |
| **Executor** | Checks current state, prepares a candidate, validates it, and commits it. Declared asynchronous effects run after commit and return events to the machine. |

The basic application loop is:

```rust
// Your application gathers the evidence and constructs `state`.
let evaluation = controller.evaluate(&state).await;
let outcome = executor.execute(evaluation).await?;
```

Pass the entire evaluation result to the executor. The machine can define transitions for inference failures as well as successful recommendations. Inspect the returned outcome to see whether the transition was applied or rejected, and whether any effects completed successfully.

State transitions commit in memory; external effects run afterward. See [execution semantics](SDK_README.md#guarantees-and-current-scope) for persistence and failure handling.

## Quick example: a circuit breaker

Requires **Rust 1.92 or newer**. From a checkout of this repository, run the included example:

```sh
cargo run -p reflex --example circuit_breaker
```

This example runs locally without credentials or network access. It uses a deterministic judge so you can see the execution behavior before connecting a model. The circuit moves through **Closed → Open → HalfOpen → Closed**: it stops normal traffic under distress, permits one recovery probe, and closes after that probe succeeds.

The following excerpt shows its transition rules. The data types and hook implementations are in the [complete runnable example](crates/reflex/examples/circuit_breaker.rs).

```rust
let definition = state_machine! {
    phase: Phase,
    data: CircuitData,
    action: CircuitAction,
    event: ProbeEvent,
    invariants: [valid_counts, valid_phase],
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
    ],
};
```

The judge can recommend a probe, but the cooldown guard must pass before the executor reserves it. The probe runs as an effect **after** that reservation commits. Its completion event identifies the probe, allowing the next guard to reject a stale result. Invariants check that the phase and runtime data remain consistent.

The complete example also defines behavior for evaluation errors and prints the execution receipts. See the [SDK guide](SDK_README.md) for constructing the executor, hook signatures, and handling outcomes.

## Use Reflex in your application

The crates are under development and are **not yet published to crates.io**. Add a path dependency on your checkout, adjusting the path for your project:

```toml
[dependencies]
reflex = { path = "../reflex/crates/reflex" }
tokio = { version = "1", features = ["rt-multi-thread", "macros"] }
```

Start by defining your state and action types, implementing a judge, and declaring the machine's transitions. Keep guards and updates short and free of external side effects. Use declared effects for network requests or other I/O.

To use **Jev**, add the `typesafe-ai` and `reflex-typesafe` crates from the same checkout. Construct a standalone `TypeSafeClient`, define a typed task, and inject both into `TypeSafeJudge`. Your executor and its rules remain application-owned. The [live Jev example](crates/reflex-typesafe/examples/jev.rs) shows the client and controller setup:

```sh
# Set TYPESAFE_API_KEY in your environment before running.
# This command makes a real provider request.
cargo run -p reflex-typesafe --example jev
```

That example evaluates a recommendation; the circuit-breaker example above demonstrates execution. The [TypeSafe integration guide](SDK_README.md#typesafe-integration) explains how to connect them.

## Explore the interactive systems

The repository includes a local playground for circuit breaking, resource scheduling, and retry/recovery. Start it without an API key to use deterministic policies:

```sh
cargo run -p reflex-sim --locked -- --playground
```

With `TYPESAFE_API_KEY` set, enable live Jev recommendations:

```sh
cargo run -p reflex-sim --locked -- --playground --policy jev
```

Inject failures, change client traffic, adjust scheduling priorities, and inspect decisions and guard outcomes. The scheduler includes per-client lag charts so you can see how waiting times change during a run.

The simulations can query **Datadog** for observed state. An application-supplied forecasting provider can add forecasts to Jev’s evidence; no live forecasting adapter is bundled. These integrations belong to the application layer, outside the core SDK.

- [Playground setup, scenarios, and simulation model](crates/reflex-sim/README.md)
- [Forecasting interface and actual-versus-forecast charts](crates/reflex-sim/FORECASTING.md)
- [Datadog telemetry and dashboards](dashboards/README.md)
- [Capacity research: workloads, algorithms, methods, and results](studies/capacity/README.md)

## Further reading

- [SDK quickstart and API behavior](SDK_README.md)
- [Core concepts and circuit-breaker walkthrough](REFLEX_SDK.md)
- [Declarative state-machine design](DECLARATIVE_EXECUTOR.md)
- [Core metrics, traces, and logs](crates/reflex/TELEMETRY.md)
- [Standalone TypeSafe client instrumentation](crates/typesafe-ai/TELEMETRY.md)

To run the workspace tests:

```sh
cargo test --workspace --all-targets --locked
cargo test --workspace --doc --locked
```
