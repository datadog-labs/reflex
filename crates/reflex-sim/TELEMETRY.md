# Circuit-breaker telemetry

The circuit-breaker playground publishes HTTP client, HTTP server, and breaker metrics through OpenTelemetry. The application owns its exporter and subscriber. Initialize the global meter provider before constructing the playground, or pass an explicit meter to `LiveSimulation::with_policy_and_meter`.

The three receiving services are `catalog`, `payments`, and `search`. Client and breaker metrics identify the target with `upstream`. Server metrics identify the receiver with `service`. All metrics include `policy` (`threshold` or `jev` in the playground). Deployment tags come from the application's exporter resource.

## Metrics

| Name | Type | Measurement |
|---|---|---|
| `http.client.requests` | Counter | One increment per terminal client attempt |
| `http.client.request.duration` | Histogram, seconds | Arrival until response, timeout, or local circuit rejection |
| `http.client.in_flight` | Gauge | Attempts without a terminal client outcome |
| `http.server.requests` | Counter | One increment per server completion or full-queue rejection |
| `http.server.request.duration` | Histogram, seconds | Arrival through server completion, including queue wait |
| `http.server.queue.depth` | Gauge | Requests waiting for a worker |
| `http.server.queue.wait` | Histogram, seconds | Queue wait, recorded when a worker starts the request; immediate starts record zero |
| `http.server.active` | Gauge | Requests occupying workers |
| `http.server.utilization` | Gauge, fraction | Occupied workers divided by worker capacity |
| `http.server.timed_out_work` | Gauge | Active requests whose clients have timed out |
| `circuit_breaker.state` | Gauge | `state:closed`, `state:open`, and `state:probe`; the current state is 1 and the others are 0 |
| `circuit_breaker.transitions` | Counter | Actual state changes, tagged `from` and `to` |
| `circuit_breaker.probes` | Counter | One increment at the client outcome of a recovery probe, tagged `outcome:success`, `error`, or `timeout` |

Client request and duration metrics carry `error` and `outcome:success`, `http_error`, `timeout`, or `circuit_open`. Server request and duration metrics carry `error`. Both sides carry `http.method:GET`; these workloads explicitly model GET requests. `http.status_code` is included only when an HTTP response exists.

The model produces HTTP 200 for successful server work, HTTP 500 for processing failure, and HTTP 503 when the server queue is full. A client timeout or locally blocked request has no HTTP status code. Local blocking does not increment the server counter. Full-queue rejection increments both counters and records zero duration; it never reaches a worker, so it has no queue-wait sample.

A late HTTP 200 can increment the server success counter after the client already recorded a timeout. That request does not generate a second client outcome or a second probe outcome. Queued requests that time out retain their place; the timed-out-work gauge counts them once they occupy workers.

## Clock and incident lifecycle

Durations use simulated seconds. Metrics are exported with real collection timestamps. Datadog rates derived from the completion counters therefore describe completions per wall-clock second and reflect playback speed. API latency in `typesafe.client.*` continues to use real time.

Gauges are observed at export time from the latest simulation snapshot, including while paused. Ending or replacing an incident releases its gauge observations. Counters remain cumulative across resets within the process. One current incident per policy should use a given meter/tag combination.

Interactive replay suppresses these HTTP and breaker metrics to avoid counting the same incident again. Reset or switch policy to start a new instrumented incident. Offline comparison reports do not emit these metrics by default; `simulate_with_meter` explicitly enables them for a batch run. Reflex SDK instrumentation still covers any controller/machine work performed during replay or batch execution.

## Logs and traces

The `reflex_sim` tracing target emits structured INFO logs for fault edits, repairs, play, pause, reset, replay, policy changes, and actual breaker state changes. Transition logs include the target, previous/new state, trigger, and simulation time. Each Jev decision has a `circuit_breaker.decision` parent span containing `upstream`, decision ID, breaker state, and simulation time. This span stays alive across background inference, any wait while paused, and guarded application on the simulation clock:

```text
circuit_breaker.decision
  circuit_breaker.evaluate
    reflex.evaluate
      typesafe.system_one
        typesafe.http_attempt
  circuit_breaker.apply
    reflex.execute (clock update)
    reflex.execute (guarded proposal or evaluation-error handling)
```

The runtime carries both span and subscriber context through the pending-result queue and into spawned work. Reset or cancellation closes the decision with `status:cancelled`; inference errors and rejected proposals retain the same trace. Context never enters model evidence or replay JSON. Independent traffic observations remain separate operations; they are not descendants of a Jev decision. New traces use this structure; previously exported traces cannot be repaired. Simulated requests do not create spans with invented wall-clock durations. Request IDs and changing simulation timestamps are not metric tags.

## Direct Datadog export

Set `DD_API_KEY`, `TYPESAFE_API_KEY`, `DD_SITE` (default `datadoghq.com`), `DD_SERVICE=reflex`, and `DD_ENV=local` in the process environment, then run:

```sh
cargo run -p reflex-sim --locked --example datadog_playground -- --port 8743 --duration-secs 120 --max-evaluations 60
```

Open `http://127.0.0.1:8743/`, press Play, inject faults, and repair the services. The server starts paused and stops after the wall-clock limit, then flushes metrics, traces, and logs. The Jev limit applies per incident. The runner does not load an env file automatically.

The runner shares the [application-owned Datadog exporter](../typesafe-ai/examples/support/datadog.rs), which uses authenticated OTLP HTTP/protobuf ingestion without an Agent. See the [client telemetry setup](../typesafe-ai/TELEMETRY.md) for exporter details. The normal CLI continues to use the application's configured telemetry providers and does not enable Datadog export automatically.

Example metric queries:

```text
sum:http.client.requests{env:local} by {upstream,outcome}.as_count()
sum:http.server.requests{env:local} by {service,http.status_code}.as_count()
max:http.server.queue.depth{env:local} by {service}
max:circuit_breaker.state{env:local,state:open} by {upstream}
```

Use the client process's service tag for client/SDK signals and the receiver's service identity for server signals.

## Use Datadog as Jev's evidence source

Start the instrumented runner with `--datadog-evidence` to query Datadog instead of collecting local client samples for Jev:

```sh
# Supply DD_API_KEY, DD_APP_KEY, TYPESAFE_API_KEY, DD_SITE and DD_ENV in the environment.
cargo run -p reflex-sim --locked --example datadog_playground -- \
  --datadog-evidence --port 8743 --duration-secs 600 --max-evaluations 60
```

`DD_APP_KEY` must permit `timeseries_query`. Credentials stay on the server; they never enter the browser, evidence, logs or recordings. The runner reads the process environment, not an env file automatically. The flag enables Datadog evidence for circuit breaking, [resource scheduling](SCHEDULER_TELEMETRY.md), and [retry/recovery](RECOVERY_TELEMETRY.md). Without the flag, circuit breaking continues to use local evidence.

Datadog mode runs at 1× real time for up to ten minutes. Pause cancels pending work; resume requires a new collection window. Step, accelerated playback, replay and switching to the threshold policy are disabled in this mode. Other tabs retain their normal controls. The circuit breaker's phase, revision, timeout, cooldown and single-probe reservation remain authoritative in Reflex. A successful probe still closes automatically, and a failed probe reopens immediately.

The UI identifies Datadog as the source, reports warm-up/query failures and evidence age, and shows the exact model input in the decision inspector. Exported decision records include that input and the data actually queried. Query failures retain the circuit state and do not call Jev or consume its budget. In-flight Jev calls remain counted if the session is paused. Reset creates a new run and a fresh per-run call budget; cumulative inference cost remains visible.

### Query scope and timing

Every HTTP and breaker metric in this mode has a `simulation_run` tag. Queries filter that run, `env`, `policy:jev`, and the upstream/receiving service. Reset rotates the run identifier. This prevents prior runs, the scheduler, and the recovery workload from supplying evidence to the breaker. The tag creates one metric-series family per incident; use this run isolation for the playground, and a stable deployment/instance scope for production systems.

The driver performs at most one evidence fetch at a time, round-robin across the three services, at least ten wall-clock seconds apart. It uses Datadog's v2 timeseries endpoint for counts and gauges. Optional v2 scalar queries obtain each window's aggregate p95, rather than averaging bucket percentiles. Percentile queries can fail independently: latency stays `null` with an explicit status, while otherwise usable count/gauge evidence remains available. Enable percentile aggregations on the client-duration distribution to obtain p95.

Queries request 10-second buckets ending at least 20 seconds behind real time to allow for ingestion. Returned bucket spacing is checked; coarser uniform buckets up to 30 seconds are accepted, with actual window durations reported. The short window includes enough complete buckets to cover at least 30 seconds; the long window covers up to 120 seconds. Both may be shortened by the incident/transition boundary, but never below 30 seconds. Sparse or malformed results fail closed instead of inventing measurements.

Start, resume and the latest transition to closed establish a boundary. The opening window begins at least 20 seconds after that boundary to exclude batches that may contain previous-state observations. With export, ingestion and polling delays, warm-up typically needs roughly 70–100 seconds; actual readiness depends on the returned data. When open, probing can use fresh run evidence without waiting for a new post-close sample window. The existing three-second cooldown still applies.

Missing outcome series are treated as zero only when the sum of the available exhaustive outcomes matches the independently queried total for every bucket. Missing total buckets or inconsistent counts invalidate the evidence. Locally blocked requests are reported separately and excluded from both failure ratios and latency queries. Zero responses produce an unknown failure ratio, not an assertion of health.

Every gauge includes its own timestamp. The executor rejects missing/invalid evidence, data older than 60 seconds, query results older than ten seconds, proposals older than ten simulation seconds, and proposals from an earlier circuit revision. Opening additionally requires at least ten Datadog responses in a complete window after the latest close. No local sample-count fallback is used. Query failure, rate limiting, or loss of Datadog connectivity retains the current plan.

### Model input

```text
service
control
  phase, revision, cooldown_remaining_ms, client_timeout_ms, legal_actions
telemetry
  source, status, simulation_run, fetched_at_unix_ms, latest_data_age_seconds
  short_window / long_window
    start_unix_ms, end_unix_ms, duration_seconds
    responses, successes, http_errors, timeouts, blocked_requests
    failure_ratio, p95_latency_ms
  server
    queue_depth, active_requests, utilization, timed_out_work, client_in_flight
    (each measurement has value and observed_at_unix_ms)
  latency_status
```

The local 1-second/5-second windows are not included in this input. Datadog fetching runs asynchronously inside the decision trace under `datadog.evidence`, alongside the existing Jev evaluation and guarded application. Traffic continues while the query and model call are pending.

API contracts: [Datadog timeseries queries](https://docs.datadoghq.com/api/latest/metrics/query-timeseries-data-across-multiple-products/), [scalar queries](https://docs.datadoghq.com/api/latest/metrics/query-scalar-data-across-multiple-products/), and [distribution percentiles](https://docs.datadoghq.com/metrics/distributions/).
