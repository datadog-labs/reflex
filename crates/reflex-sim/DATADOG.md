# Datadog telemetry control loop

The circuit-breaker, scheduler, and recovery applications publish OpenTelemetry metrics, traces, and logs directly to Datadog's OTLP intake. No Agent is required. In Datadog state mode, they query those metrics through the Datadog API, validate the result, and pass typed observations to Jev. Reflex checks the recommended action against the application's current state.

## Run

From the repository root:

```sh
npm ci --prefix crates/reflex-sim/ui
npm run build --prefix crates/reflex-sim/ui
cp .env.example .env.local
# Fill in your keys and site in .env.local, then:
set -a
. ./.env.local
set +a
cargo run -p reflex-sim --features datadog --locked -- \
  --playground --policy jev --datadog --datadog-evidence
```

`--datadog` enables export. `--datadog-evidence` also enables queries for all three tabs and requires the Jev policy. Without either flag, the existing local playground works without Datadog credentials. `--datadog` alone works with the default deterministic policy and only needs `DD_API_KEY`.

| Variable | Purpose |
| --- | --- |
| `DD_API_KEY` | Authenticate telemetry export and metric queries |
| `DD_APP_KEY` | Authorize metric queries; requires `timeseries_query` |
| `DD_SITE` | Account site, default `datadoghq.com` |
| `DD_SERVICE` | Application resource service, default `reflex` |
| `DD_ENV` | Exported environment and query filter, default `local` |
| `TYPESAFE_API_KEY` | Live Jev inference |

Use `--port` to change the local port and `--jev-max-evaluations` to bound inference calls per incident. The browser receives neither Datadog nor TypeSafe credentials. The process runs until Ctrl+C; shutdown stops the simulations and flushes all three telemetry pipelines. The older `datadog_playground` example remains available for time-bounded runs.

## See the round trip

1. Open a scenario and start traffic. Choose an incident preset or inject a failure after warmup.
2. The Datadog panel shows the run ID and query status. Export is periodic, not synchronous with each request. Startup text confirms configuration, not successful ingestion.
3. In Datadog, import the corresponding [dashboard](../../dashboards/README.md), choose `env`, `policy:jev`, and that `simulation_run`. Each scenario has its own run; resets create a new ID.
4. Inspect a decision in the playground's Activity tab. The evidence includes `telemetry.source: "datadog"`, observation times, and the values used by Jev. Circuit-breaker details show the exact model input; scheduler and recovery serialize their Datadog observations alongside application-owned controls.
5. Change traffic or inject faults. Metric changes arrive in Datadog, later queries expose them to Jev, and Reflex accepts or rejects the recommendation using current guards.

Charts and topology display the simulator's current state. They are not Datadog charts. Traces and logs help explain requests and decisions but are not queried as decision inputs.

## Metrics and state

| Application | Representative exported metrics | Queried evidence |
| --- | --- | --- |
| Circuit breaker | `http.client.requests`, `http.client.request.duration`, `http.server.requests`, `http.server.queue.depth`, `http.server.active`, `http.server.utilization`, `circuit_breaker.state` | Client outcomes and p95 latency, queues, active/timed-out work, utilization, client in-flight work |
| Resource scheduler | `scheduler.jobs`, `scheduler.queue.depth`, `scheduler.queue.oldest_age`, `scheduler.node.cpu.reserved`, `scheduler.node.memory.reserved`, `scheduler.node.jobs.running` | Queue depth and age per client; running jobs and CPU/memory capacity/reservations per node |
| Recovery | `http.client.requests`, `http.server.requests`, `recovery.replica.state`, `recovery.replica.heartbeat.age`, `recovery.retries`, `recovery.rebuilds` | Client outcomes, latency, per-replica health/queues/outstanding work, heartbeat and serving status |

Request counters combine successes and failures using outcome/error/status tags. SDK metrics cover inference latency, outcomes, and token usage, plus Reflex evaluations and transitions. Trace context connects decisions with provider calls and execution. See [dashboard notes](../../dashboards/README.md) for tags and metric semantics.

Circuit-breaker client metrics use `upstream:catalog|payments|search`; server metrics use receiving `service:catalog|payments|search`. Do not apply `service:reflex` to all server queries. Scheduler and recovery queries also scope their client/node/replica series to the current run. SDK-wide metrics are not run-scoped.

## Delay and guards

Metric export runs every 10 seconds. Queries use completed buckets with a 20-second ingestion margin. Circuit-breaker and recovery evidence require at least 30 seconds of valid observations and can summarize up to 120 seconds; scheduler evidence uses recent complete node/client gauge samples. Allow roughly one to two minutes after starting traffic, depending on ingestion. A stalled pipeline can take longer or remain unavailable.

Samples older than 60 seconds are rejected. Query results also have a short cache lifetime. Missing series, incomplete windows, invalid values, or changed run IDs prevent evaluation; an empty response does not mean zero failures or an empty queue. Pause/resume clears cached evidence and requires fresh observations. Datadog mode disables accelerated playback, stepping, replay, and switching to local policies.

Current circuit phases, job identities/priorities, replica lifecycle, budgets, and legal choices remain application-owned controls. Reflex rechecks revisions, cooldowns, capacity, and other guards at execution time. This prevents a delayed observation from authorizing an illegal transition.

## Troubleshooting

- **Build requires `datadog`:** include `--features datadog` before Cargo's `--` separator.
- **Waiting for observations:** start traffic, keep 1× playback running, and verify the run's metrics in Datadog. Resetting or resuming restarts warmup.
- **401/403 queries:** verify both keys belong to the selected site and that the application key has metric-query permission.
- **Unavailable latency:** enable percentile aggregations for the request-duration distributions. Optional latency remains explicitly unavailable rather than becoming zero.
- **No new evaluations:** inspect telemetry status, pause state, legal choices, and the inference-call budget. Some actions are deterministic and require no model call.

The adapters use the public [timeseries query API](https://docs.datadoghq.com/api/latest/metrics/query-timeseries-data-across-multiple-products/). See Datadog's [OTLP intake documentation](https://docs.datadoghq.com/opentelemetry/setup/otlp_ingest/) for account-side ingestion setup.
