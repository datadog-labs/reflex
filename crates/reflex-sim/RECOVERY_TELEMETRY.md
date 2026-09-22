# Recovery telemetry

The recovery simulation publishes request, retry, replica, and rebuild metrics through OpenTelemetry. `Session::new` and `Engine::new` use the global `recovery` meter. `Session::with_meter` and `Engine::with_meter` accept an application-owned meter for explicit configuration and testing. The application owns providers, exporters, and shutdown.

## Metrics

| Name | Type | Recorded value |
| --- | --- | --- |
| `http.client.requests` | Counter | One final result per original request, including rejection |
| `http.client.request.duration` | Histogram, seconds | Arrival to final result, including every retry |
| `http.client.in_flight` | Gauge | Original requests awaiting their final result, per client |
| `http.server.requests` | Counter | One terminal result per attempt received by a replica |
| `http.server.request.duration` | Histogram, seconds | Receipt to the modeled attempt result, including queue wait |
| `http.server.queue.depth` | Gauge | Received requests waiting for a worker, per replica |
| `http.server.queue.wait` | Histogram, seconds | Receipt to a worker slot or termination while queued; immediate rejection records zero |
| `http.server.active` | Gauge | Received requests occupying worker slots, per replica |
| `recovery.retries` | Counter | One retry decision after a failed attempt, with `outcome:attempted` or `suppressed` |
| `recovery.retry.budget.available` | Gauge | Remaining retry credits, including fractional credits |
| `recovery.retry.enabled` | Gauge | Retries enabled, 0 or 1 |
| `recovery.replica.state` | Gauge | 0 or 1 for each replica/state pair: empty, rebuilding, checking, ready, unavailable |
| `recovery.replica.serving` | Gauge | Replica included in the serving pool, 0 or 1 |
| `recovery.replica.reachable` | Gauge | Last observed reachability, tagged `path:client` or `transfer` |
| `recovery.serving.essential_only` | Gauge | Optional traffic rejected, 0 or 1 |
| `recovery.intervention.required` | Gauge | Operator intervention requested, 0 or 1 |
| `recovery.rebuilds` | Counter | One terminal rebuild result: completed, failed, cancelled |
| `recovery.rebuild.duration` | Histogram, seconds | Start to terminal result, including verification |
| `recovery.rebuild.active` | Gauge | 0 or 1 for each source/target/phase combination; phases rebuilding and verifying |
| `recovery.rebuild.bytes` | Counter | Actual transferred bytes, including partial failed/cancelled transfers; decimal MB converted to bytes |

Every metric carries `policy:jev|fixed` and `upstream:replica_pool`. Client metrics identify `client:client_1`, etc. Server metrics carry `service:replica_pool` and `replica:replica_a`, etc. Deployment `env`, service and version come from the configured exporter; the server metric's service identifies the receiving service. Request result and duration metrics also carry `essential` and `error`.

Client results have `outcome:success|failure|rejected` and `retried:true|false`. Failure/rejection reasons are `server_error`, `timeout`, `queue_full`, `no_serving_replica`, or `essential_only`. Server results have `outcome:success|server_error|timeout|queue_full` and `retry:true|false`. A modeled response carries `http.status_code:200|500|503`. Transport timeouts, admission rejections, and requests that never reached a replica do not get invented HTTP status codes.

Suppressed retry reasons are `attempt_limit`, `disabled`, `budget_exhausted`, or `no_alternative`, checked in that order. `attempted` means a retry was initiated and consumed a credit; routing can still reject it for a full queue. Rebuild metrics carry `source` and `target` replica labels. IDs and raw error messages are excluded from metric tags.

## Accounting and simulation time

A failed initial attempt followed by a successful retry records two server results and one successful client result. A request prevented from reaching any server by a crash or partition records no server result. If connectivity returns while the attempt is pending, receipt is recorded then. Worker occupancy includes received work blocked by a later fault. Queue waits are recorded once at worker assignment or termination while queued.

This model ends an attempt at its simulated deadline and does not model independent late server completion. Its server timeout series means received work terminated at that deadline, not an HTTP response. Rate formulas comparing server attempts to original requests must account for attempts that never reached a replica.

Telemetry is emitted from committed transitions, including intermediate ticks. Reads, exports, invalid actions and rejected guards do not repeat metric events. Internal timing/event records are omitted from JSON exports and Jev evidence. Paused sessions retain current gauges. Reset replaces the snapshot, preserves cumulative counters, and does not report unfinished work as completed or cancelled. At the simulation horizon, outstanding work remains outstanding.

All durations are **simulated seconds**. Export cadence and Datadog counter rates use wall-clock time; increasing simulation speed increases the reported wall-clock request rate. Rebuild throughput from the byte counter is also per wall-clock second. No additional rate or success-percentage metrics are necessary.

## Logs and traces

Committed fault/control changes, recovery actions, decision outcomes and rebuild terminal results produce structured logs. Existing Reflex instrumentation reports guard rejection and evaluation/effect errors. Existing TypeSafe instrumentation reports request latency, retries, tokens and failures.

The decision context survives background inference, pauses, and application on a later clock tick:

```text
recovery.decision
├── recovery.evaluate
│   └── reflex.evaluate
│       └── typesafe.system_one
│           └── typesafe.http_attempt
└── recovery.apply
    └── reflex.execute
```

Root status is applied, unchanged, rejected, evaluation_error, error, or cancelled. Reset and in-flight cancellation close the root under its original trace context. Fixed-policy decisions have an apply branch without inference. The core controller and machine are named `retry_recovery`. Per-request simulation spans with fabricated wall-clock timing are not emitted.

## Direct Datadog export

Set `DD_API_KEY` and `TYPESAFE_API_KEY` in the process environment. Optional settings include `DD_SITE`, `DD_SERVICE`, `DD_ENV`, and `TYPESAFE_MODEL`. The same example enables Jev for circuit breaking, scheduling and recovery:

```sh
cargo run -p reflex-sim --example datadog_playground -- \
  --port 8743 --duration-secs 120 --max-evaluations 60
```

Open `http://127.0.0.1:8743/recovery`, select a scenario, then press Play. Tabs start paused; only run the simulations you want to inspect. Each has its own inference-call budget. The runner flushes metrics, traces and logs on timed shutdown.

Useful filters: `upstream:replica_pool`, `policy:jev`, and your configured `env`. Search traces for operation `recovery.decision`. Use `http.client.requests{error:false}` / all client results for client success, and compare retried successes against `recovery.retries{outcome:attempted}` to assess retry benefit. The exporter uses the Datadog API directly; no agent is required.

## Datadog evidence mode

Run the instrumented playground with `--datadog-evidence` and `DD_APP_KEY` granting `timeseries_query`. The flag enables Datadog evidence for circuit breaking, resource scheduling and recovery. The process also needs the API/model keys described above. The runner does not load env files automatically.

Recovery adds these metrics:

| Metric | Meaning |
| --- | --- |
| `recovery.replica.requests.outstanding` | Gauge, original requests currently assigned to each replica, including attempts not yet received because of a partition/crash |
| `recovery.replica.heartbeat.age` | Gauge, simulated seconds since the last observed heartbeat, by replica |

Final client result/duration metrics now carry `replica` when a destination was assigned, including queue-full rejections. Essential-only admission and no-serving-replica rejections have no replica tag; the simulator's placeholder node is never exported as attribution. A retried client's final result belongs to its final assigned replica; this is distinct from per-attempt server attribution. All recovery metrics in Datadog mode carry `simulation_run`, rotated on reset/scenario change. Removed clients continue publishing zero in-flight gauges once their requests drain.

### Model input

```text
control
  observed_at_ms, revision
  desired_replicas, required_snapshot_version
  configured_arrival_rate, configured_essential_fraction
  replicas: id, name, phase, serving, snapshot_version
  essential_only, retries_enabled, retry_credit, retry_limit
  bandwidth_limit_mb_s
  recovery: id, source, target, phase, configured_rate_mb_s (or null)
  intervention_requested, last_action_age_ms, recent_actions, legal_choices
telemetry
  source: datadog
  simulation_run, fetched_at_unix_ms, latency_status
  short_window / long_window
    start_unix_ms, end_unix_ms, duration_seconds
    all / essential
      responses, successes, failures, rejected, success_rate
      p95_success_latency_ms
    replicas: replica, outcomes (same fields, or null if incomplete)
    retry_attempts, suppressed_retries, rebuild_bytes_per_second
  replicas
    replica
    outstanding_requests, server_queue_depth, server_active
    heartbeat_age_seconds, client_reachable, transfer_reachable
    (each gauge has value and observed_at_unix_ms)
```

The model input omits local 1/5-second health windows, local per-replica response/load/reachability observations, and exact rebuild progress/timing. The operation identity, lifecycle, configured rate, verified snapshot versions, legal actions and safety budgets remain authoritative in the executor. Configured traffic rate/mix is explicitly labeled configuration, not a measured arrival rate. The topology, live cards, and `view.evidence` continue to describe the simulator; each decision's `evidence` is the actual model input and identifies Datadog as its source.

### Query and execution contract

One background evidence fetch runs at a time, at least ten wall-clock seconds between starts. Each fetch makes one batched timeseries query and up to two scalar percentile queries. Counts are scoped by environment, application service, `upstream:replica_pool`, `policy:jev`, and run. Server queue/active queries use receiving `service:replica_pool`. Gauges disable interpolation and expose their own timestamps; their values are maxima within ten-second buckets.

Windows exclude the newest twenty seconds for ingestion. The short window is thirty seconds; the long window extends up to 120 seconds, clipped to the current run/resume boundary. Both require at least thirty seconds of complete aggregate client counts. After start/resume, only buckets at least twenty seconds after that boundary are eligible. Warm-up typically takes 70–100 seconds depending on ingestion.

Exhaustive essential/outcome counts must match independently queried totals in every bucket before missing outcome categories can mean zero. Replica outcome categories also reconcile against independently queried replica totals; a replica without complete totals has unknown outcomes. Optional retry/byte counters with incomplete buckets remain null. Zero responses give a null success rate, not healthy status. Optional p95 values use successful-client distribution samples over each whole window; percentile failure produces null values and does not invent zero latency.

Query results expire after fifteen seconds. Count windows and individual gauges must remain within sixty seconds of real time. Unavailable, malformed, stale or wrong-run required evidence retains the current plan without calling Jev; no local-health fallback is used. At application time freshness/run are revalidated before Reflex's existing five-second proposal-age, revision, cooldown, eligibility, snapshot-verification and resource-budget checks.

This mode uses continuous 1× playback. Step, accelerated playback and the fixed policy are disabled. Pause/reset/horizon cancel pending fetches and inference; resume requires fresh observations. Fault and bandwidth edits continue to affect the authoritative system immediately and can invalidate pending proposals through the revision guard. The simulator still has a three-minute horizon, so short canned faults may end during initial warm-up. For observing a recovery decision from Datadog, use Sandbox and keep the injected failure active through warm-up.

Import [retry-recovery.json](../../dashboards/retry-recovery.json) into a new Datadog dashboard; [dashboard notes](../../dashboards/README.md) cover filters, scope and verification data.
