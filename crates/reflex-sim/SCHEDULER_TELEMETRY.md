# Resource scheduler telemetry

The scheduler emits metrics through the application's OpenTelemetry meter provider and structured logs/traces through `tracing`. `Session::with_meter` accepts an explicit meter; `Session::new` captures the global meter when constructed. The scheduler installs no exporter.

## Metrics

| Name | Type | Meaning |
|---|---|---|
| `scheduler.jobs` | Counter | Each job once at completion or rejection; `outcome:completed/rejected`, `error`, and rejection `reason:too_large/queue_full` |
| `scheduler.job.duration` | Histogram, seconds | Arrival to terminal outcome, including queue time |
| `scheduler.job.run.duration` | Histogram, seconds | Actual execution time of completed jobs |
| `scheduler.job.cpu` | Histogram, CPU units | Requested CPU, once per arrival, including rejected arrivals |
| `scheduler.job.memory` | Histogram, bytes | Requested memory, once per arrival, including rejected arrivals |
| `scheduler.queue.depth` | Gauge | Queued jobs per client |
| `scheduler.queue.wait` | Histogram, seconds | Wait recorded once on successful placement; immediate placement records zero |
| `scheduler.queue.oldest_age` | Gauge, seconds | Age of the oldest queued job per client; zero for an empty queue |
| `scheduler.placements` | Counter | Processed placement attempts with `outcome:placed/deferred/rejected/evaluation_error` and `error` |
| `scheduler.node.jobs.running` | Gauge | Running jobs per node |
| `scheduler.node.cpu.capacity` | Gauge, CPU units | Total node CPU capacity |
| `scheduler.node.cpu.reserved` | Gauge, CPU units | CPU reserved by running jobs |
| `scheduler.node.memory.capacity` | Gauge, bytes | Total node memory capacity |
| `scheduler.node.memory.reserved` | Gauge, bytes | Memory reserved by running jobs |

Every metric carries `policy:jev/first_fit/best_fit`. Job and queue metrics identify the source with `client:client_1`, etc. Node metrics identify `node:node_a` through `node_d`; placement metrics include a selected node when present, and job completion/run/wait metrics include the assigned node. These join the application's service/environment resource tags. Job IDs are restricted to logs and traces.

A deferred placement leaves the job queued and is not an error. Guard rejection or inference failure increments a placement outcome without terminating the job. A job too large for any node or arriving at a full queue is terminally rejected; its requested CPU/memory are still recorded. Jobs otherwise run to completion; the model does not simulate execution failures. Actual durations use completion timestamps and include the seeded runtime variation, rather than the duration estimate shown to Jev.

Memory is converted from model GiB to bytes (1 GiB = 1,073,741,824 bytes). Node utilization can be computed as reserved divided by capacity; these are reservations, not measured CPU activity. Queue totals are the sum across clients, and the global oldest age is the maximum. Removed clients' queued work remains visible until placed.

## Time, pause, and reset

Duration metrics use simulated seconds. Export timestamps and rates derived from counters use wall-clock time, so playback speed affects those rates. API latency remains real elapsed time. Jobs still queued or running at the simulation horizon have no terminal outcome yet.

Observable gauges preserve the current snapshot while paused. Reset or policy changes release the old gauges and start a fresh snapshot without inventing completed/cancelled jobs. Counters and histogram samples remain cumulative within the process, separated by policy; a job or placement is never counted again simply because state is read or exported. Counter/histogram updates are based on committed state after each event.

## Traces and logs

```text
scheduler.decision
  scheduler.evaluate
    reflex.evaluate
      typesafe.system_one
        typesafe.http_attempt
  scheduler.apply
    reflex.execute
```

One decision trace spans background Jev inference, any paused wait, and guarded placement. The shared context helper also handles reset/cancellation. Baseline policies use the same decision/application trace without a Jev evaluation. Inference failures and rejected placements retain their decision trace. Span context stays out of model evidence and exported recordings.

The `reflex_sim::scheduler` target logs placement outcomes, terminal rejections, and client/playback controls. Existing `reflex.*` counters and spans carry `controller:resource_scheduler` or `machine:resource_scheduler`; existing `typesafe.client.*` telemetry covers provider requests, latency and errors. No additional inference metrics duplicate these.

## Datadog runner

With `DD_API_KEY`, `TYPESAFE_API_KEY`, `DD_SERVICE=reflex`, and `DD_ENV=local` in the process environment:

```sh
cargo run -p reflex-sim --locked --example datadog_playground -- --port 8743 --duration-secs 120
```

Open `http://127.0.0.1:8743/scheduler`. Both playgrounds start paused; the runner enables live Jev for the scheduler and circuit breaker, and flushes all telemetry when its wall-clock limit expires. Each playground has its own call budget, reset per incident. It does not load env files automatically.

Example Datadog queries:

```text
sum:scheduler.jobs{service:reflex,env:local} by {client,outcome}.as_count()
sum:scheduler.placements{service:reflex,env:local} by {policy,outcome}.as_count()
sum:scheduler.queue.depth{service:reflex,env:local} by {policy}
max:scheduler.queue.oldest_age{service:reflex,env:local} by {client}
max:scheduler.node.cpu.reserved{service:reflex,env:local} by {node}
```

## Datadog as the scheduler's evidence source

Add `--datadog-evidence` and provide `DD_APP_KEY` with `timeseries_query` permission to the instrumented runner. The flag enables Datadog evidence for circuit breaking, scheduling, and recovery. Credentials stay in the server process.

The scheduler supplies the exact FIFO candidate, up to eight next queued jobs, and current legal choices. Datadog supplies delayed CPU/memory reservations and capacities, running-job counts, and per-client queue depths/oldest ages. The model receives:

```text
request
  candidate: id, client, cpu, memory_gib, estimated_duration_ms, waiting_ms
  upcoming_jobs: up to eight jobs with the same fields
control
  observed_at_ms, revision, legal_choices
telemetry
  source: datadog
  simulation_run, fetched_at_unix_ms, observed_at_unix_ms
  nodes: node, cpu_capacity, cpu_reserved, memory_capacity_bytes,
         memory_reserved_bytes, running_jobs
  queues: client, queued_jobs, oldest_age_seconds
```

The local node snapshot, local total queue length, and per-running-job remaining-time estimates are omitted from model input in this mode. Queue statistics and node measurements are the maxima within the same ten-second bucket; they are delayed operational context, not an atomic current scheduler snapshot. Memory remains in bytes. Historical duration distributions are available in the dashboard but are not queried for model input in this first implementation.

All scheduler metrics in this mode include `simulation_run`. Reset rotates the tag and cancels pending queries/inference. Removed clients continue exporting zero queue gauges after their work drains so historical queue values cannot linger. Queries are scoped by `env`, `service`, `policy:jev`, and run. The global SDK metrics retain their existing application scope.

At most one asynchronous Datadog fetch is active, at least ten wall-clock seconds between starts. Queries exclude the newest twenty seconds to allow ingestion, disable interpolation, and require a complete bucket for every node and relevant client. Resume and adding a client invalidate cached observations and require buckets at least twenty seconds after that boundary. Initial warm-up is typically 50–70 seconds, depending on export and ingestion timing. While paused, pending work is cancelled; resuming warms up again. Step, accelerated playback, and deterministic policies are disabled in this mode. The three-minute simulation horizon remains unchanged.

A query result expires after fifteen seconds and its observations after sixty seconds. Missing, malformed, stale, wrong-run, or unavailable evidence leaves jobs queued without calling Jev; there is no local telemetry fallback. The UI shows the evidence source, run and query status, and decision inspection/export includes the actual serialized model input. Current topology and job visualization continue using the simulator's authoritative state.

Immediately before applying a proposal, the scheduler revalidates telemetry freshness/run, then Reflex checks the current FIFO job, available CPU/memory, and the existing five-second proposal-age limit. State invariants verify reservations equal running work and stay within capacity. Datadog observations never authorize reservations by themselves.

Import [the scheduler dashboard](../../dashboards/resource-scheduler.json) into a new Datadog dashboard. See [dashboard notes](../../dashboards/README.md) for scope, units and import instructions.
