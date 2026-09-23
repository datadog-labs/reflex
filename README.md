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

The repository includes a local playground for circuit breaking and resource scheduling. Set `TYPESAFE_API_KEY` to enable Jev, then build and start the playground:

```sh
npm ci --prefix crates/reflex-sim/ui
npm run build --prefix crates/reflex-sim/ui
cargo run -p reflex-sim --locked -- --playground
```

The circuit-breaker playground uses Jev + Reflex by default. You can also select it explicitly:

```sh
cargo run -p reflex-sim --locked -- --playground --policy jev
```

Inject failures, change client traffic, adjust scheduling priorities, and inspect decisions and guard outcomes. The scheduler includes per-client lag charts so you can see how waiting times change during a run.

The simulations can query **Datadog** for observed state and run **Toto** locally to forecast demand or resource pressure. Jev receives the observations and optional forecasts; Reflex checks its recommendation against current guards. Datadog and Toto integrations belong to the playground, outside the core SDK.

- [Playground setup, scenarios, and simulation model](crates/reflex-sim/README.md)
- [Forecasting interface](crates/reflex-sim/FORECASTING.md)
- [Datadog telemetry and dashboards](dashboards/README.md)
- [Capacity research: workloads, algorithms, methods, and results](studies/capacity/README.md)

## Use Datadog telemetry as state

The playground can publish application metrics to Datadog, query them back, and use the observations as state for Jev and Reflex:

```text
Simulated services → Datadog metrics → typed observations → Jev recommendation
                                                               ↓
                                           Reflex guards → state transition
```

This works for circuit breaking and resource scheduling. The application owns the telemetry queries and builds the typed state; the core Reflex SDK remains independent of Datadog.

Build the UI as above, then copy [`.env.example`](.env.example) to `.env.local` and fill in `DD_API_KEY`, `DD_APP_KEY`, and `TYPESAFE_API_KEY`. Set `DD_SITE` to your Datadog site. The application key needs `timeseries_query` permission. Load the file and start the playground:

```sh
set -a
. ./.env.local
set +a
cargo run -p reflex-sim --features datadog --locked -- \
  --playground --policy jev --datadog --datadog-evidence
```

Click **Start traffic**. Each circuit shows whether it is waiting for telemetry or the age of its observations. Metrics export every 10 seconds; decisions wait for usable observations to arrive. Open **Activity** and inspect a decision to see the queried values and timestamps. Use the `simulation_run` in a decision’s telemetry to filter the supplied [Datadog dashboards](dashboards/README.md).

| Simulation | Observations queried from Datadog | Control state retained locally |
| --- | --- | --- |
| Circuit breaker | Request outcomes, latency, active work, queue depth, utilization | Circuit phase, revision, cooldown, legal transitions |
| Resource scheduler | Per-client queue pressure and per-node CPU/memory reservations, capacity, running jobs | Candidate jobs, priorities, FIFO/aging rules, legal placements |

Reflex rechecks each recommendation against current control state before applying it. Missing or stale telemetry prevents a model evaluation; it is not silently replaced with local measurements. The topology and live charts still show the simulator so you can compare current behavior with delayed observations. Datadog state mode uses continuous **1×** playback.

To publish metrics, traces, and logs while keeping local state, use `--datadog` without `--datadog-evidence`. Only an API key is required for publishing; TypeSafe credentials are needed when using Jev. Press **Ctrl+C** to stop and flush telemetry. See the [Datadog walkthrough](crates/reflex-sim/DATADOG.md) for metrics, warmup, and troubleshooting.

## Local Toto forecasts

The playground includes a Python service that runs [open-source Toto](https://github.com/DataDog/toto)
on your machine. It forecasts observed demand or resource pressure for circuit
breaking and scheduling. Jev receives the forecast alongside current
state; Reflex guards still determine whether an action can execute.

From the repository root, start the service in one terminal using
[uv](https://docs.astral.sh/uv/):

```sh
uv sync --project integrations/toto --python 3.12 --locked
uv run --project integrations/toto --locked reflex-toto
```

Wait for `Ready: http://127.0.0.1:8765`. The first start downloads a pinned
**Toto-2.0-22m** checkpoint. The model stays loaded and runs on CPU by default;
Toto needs no API key. Python and Toto are optional simulator dependencies.

In another terminal, with `TYPESAFE_API_KEY` set and the UI built as above,
start the playground using local observations:

```sh
cargo run -p reflex-sim --locked -- \
  --playground --policy jev --toto-url http://127.0.0.1:8765
```

To publish metrics and forecast **queried Datadog telemetry**, load the Datadog
credentials described above and run:

```sh
cargo run -p reflex-sim --features datadog --locked -- \
  --playground --policy jev --datadog --datadog-evidence \
  --toto-url http://127.0.0.1:8765
```

Each simulation has a forecasting toggle and actual-versus-forecast charts.
Forecasts cover the next 120 seconds. Ordinary local scenarios need 64 seconds
of observed history; Datadog mode needs 320 seconds plus ingestion delay. Missing
or stale forecasts leave Jev using valid observed state; missing Datadog state
still prevents evaluation.

For circuit breaking, select **Circuit Breaker → Cyclical load · Toto → Run**, then select **Payments**. The ten-minute scenario repeats a two-minute pattern: traffic rises 4× at +30s, service time rises 6× at +60s, and both recover at +90s. Toto receives observed history, not the schedule. Forecasting starts after enough observations have been collected during the run. In Datadog mode, Toto requires at least 320 seconds of history plus ingestion delay. **Actual vs Toto** compares forecasts with subsequent observations. Jev chooses when to open and probe; Reflex requires five consecutive successful probes before closing. Forecast accuracy and Jev's choices are not scripted.

For a first example, select **Resource Scheduler → Repeating demand waves · Toto
example** and click **Run scenario**. With local observations, use 4× playback;
the first forecast appears after three cycles (180 simulated seconds). In Circuit
Breaker, select a service to inspect its forecast.

See [local Toto setup](integrations/toto/README.md) for the API contract, model
selection, tests, and offline operation.

## Further reading

- [SDK quickstart and API behavior](SDK_README.md)
- [Core concepts and circuit-breaker walkthrough](REFLEX_SDK.md)
- [Declarative state-machine design](DECLARATIVE_EXECUTOR.md)
- [Core metrics, traces, and logs](crates/reflex/TELEMETRY.md)
- [Standalone TypeSafe client instrumentation](crates/typesafe-ai/TELEMETRY.md)

To run the workspace tests:

```sh
npm ci --prefix crates/reflex-sim/ui
npm run build --prefix crates/reflex-sim/ui
cargo test --workspace --all-targets --locked
cargo test --workspace --doc --locked
```

## License

Reflex is licensed under [Apache-2.0](LICENSE). See [NOTICE](NOTICE) for attribution. Third-party dependencies and assets retain their own licenses.

Third-party components are listed in [LICENSE-3rdparty.csv](LICENSE-3rdparty.csv). See the [inventory notes](third_party/README.md) for coverage, sources, and unresolved entries.

## Ownership

Owned by Datadog, Inc. See [maintenance responsibilities](MAINTAINERS.md).
