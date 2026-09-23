# Reflex telemetry

Reflex emits two OpenTelemetry counters and three `tracing` span boundaries. Structured logs carry error and rejection details. The application owns the subscriber, meter provider, export destination, and shutdown.

## Metrics

| Metric | Status values | Counting unit |
| --- | --- | --- |
| `reflex.evaluations` | `proposed`, `error`, `cancelled` | One completed or interrupted controller evaluation |
| `reflex.transitions` | `applied`, `unchanged`, `rejected`, `error` | One input processed by the built-in state-machine executor |

An applied transition committed a candidate, including updates that keep the same phase. An unchanged result made no commit. A rejected input violated a rule or had no unambiguous matching transition. An error is an executor failure, such as a panicking hook.

Effect completion events are separate inputs and produce their own transition counts. An evaluation error passed to the executor also produces a transition result: an application can accept that error through its declared fallback rule. These are different lifecycle stages, not duplicate observations of one result.

Counter attributes are only `status` and, when configured, a stable `controller` or `machine` name. Error codes, input kinds, execution IDs, and rejection stages appear in logs/traces, not metric labels. No latency, effect, storage, request, or token metrics are added here. Metrics are independent of trace sampling.

## Spans and structured logs

- `reflex.evaluate` surrounds the judge call, deadline, and confidence validation. Provider spans nest beneath it. Evaluation failures log at WARN with a bounded error category and the judge's code when available.
- `reflex.execute` surrounds a supervised submission, including its effects. It retains the caller's trace context across the spawned task and records the initial transition outcome. Rejections log at DEBUG with input kind, stage, and reason code. Executor failures log at ERROR.
- `reflex.effect` surrounds one dispatched handler. Completion logs at DEBUG; timeout, panic, and cancellation log at WARN. Each effect log includes actual elapsed seconds. A completion event's transition is processed after the effect span ends.

Effect-chain exhaustion logs at WARN. The next effect was not dispatched, so it produces no effect span. Effect failure or a rejected completion event does not undo or reclassify the original committed transition. An execution span's `status=applied` refers to that initial commit; effect spans and completion-event logs describe subsequent work.

Rejection stages include `matching`, `confidence`, `guard`, `update`, `current_invariant`, and `candidate_invariant`. Executor failures can also identify `execution` or `commit` as their stage. Telemetry is emitted after the state lock has been released; there are no storage timers or state-read metrics.

Dropping a polled controller future records one cancelled evaluation. A panicking judge still unwinds, and its evaluation counts as an error. An unpolled evaluation records nothing. Dropping the executor's awaiting caller does not cancel its supervisor or add an extra transition count. Runtime shutdown can interrupt supervised work, which logs cancellation and preserves any transition counts already emitted.

No state, action, event payload, free-form error message, or credential is logged automatically. Application types need no additional `Debug` or `Serialize` implementations. Application-supplied judge and rejection codes are recorded, so keep those codes free of secrets. Configuration/build failures return their existing errors without adding runtime counters. Custom implementations of the `Executor` trait own their instrumentation; this implementation covers `StateMachineExecutor`.

## Configure an application

Pass the same application-owned meter provider used for the TypeSafe client:

```rust
use opentelemetry::metrics::MeterProvider;

let controller = Controller::builder()
    .name("circuit_breaker")
    .meter(meter_provider.meter("reflex"))
    .judge(judge)
    .build()?;

let executor = StateMachineExecutor::builder(definition)
    .name("circuit_breaker")
    .meter(meter_provider.meter("reflex"))
    .store(store)
    .build()?;
```

Both `.name(...)` and `.meter(...)` are optional. Names take static strings to encourage stable dimensions. Without an explicit meter, initialize the global provider before building controllers and machines. The library installs no global subscriber, exporter, or background export task.

Use the [direct-to-Datadog setup](../typesafe-ai/TELEMETRY.md) for metrics, traces, and logs. Its filter includes both `reflex` and `typesafe_ai` at INFO. Enable DEBUG for `reflex` to capture expected rejections and successful effect-completion logs. Use an application parent span around evaluation and execution when both should share one trace; Reflex does not add trace IDs to proposal payloads.

In Datadog, query these counters with your application service/environment filters:

```text
sum:reflex.evaluations{service:reflex,env:local} by {status}.as_count()
sum:reflex.transitions{service:reflex,env:local} by {status}.as_count()
```

### Run the circuit-breaker playground with Datadog

With `DD_API_KEY`, `TYPESAFE_API_KEY`, `DD_SERVICE=reflex`, and `DD_ENV=local` in the process environment:

```sh
cargo run -p reflex-sim --example datadog_playground -- --port 8743 --duration-secs 120
```

Open `http://127.0.0.1:8743/`, press Play, inject faults, then repair the services. The runner starts paused, uses the live Jev policy, and exports the SDK's metrics, traces, and logs directly through the shared Datadog exporter. It installs providers before constructing any clients or state machines. The call limit applies per incident; resetting starts a new incident. The wall-clock limit stops the server and flushes telemetry. Allow that timer to finish for a confirmed final flush.

This also sends the [HTTP and circuit-breaker workload metrics](../reflex-sim/TELEMETRY.md), including client outcomes, server responses, queues, utilization, and breaker state. The normal `reflex-sim` command does not enable export automatically.
