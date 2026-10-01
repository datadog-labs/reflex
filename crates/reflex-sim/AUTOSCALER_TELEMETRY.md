# Cluster autoscaler telemetry

The autoscaler emits metrics through the application's OpenTelemetry meter provider and structured logs/traces through `tracing`. `Session::with_meter` accepts an explicit meter; `Session::new` captures the global meter when constructed. The autoscaler installs no exporter: without one (the playground without `--datadog`) nothing leaves the process.

## Metrics

| Name | Type | Meaning |
|---|---|---|
| `autoscaler.cpu.requested` | Gauge, CPU units | CPU requested by every pod, placed or pending |
| `autoscaler.memory.requested` | Gauge, bytes | Memory requested by every pod, placed or pending |
| `autoscaler.cpu.ready` | Gauge, CPU units | CPU capacity of ready nodes |
| `autoscaler.memory.ready` | Gauge, bytes | Memory capacity of ready nodes |
| `autoscaler.pods.pending` | Gauge | Pods that fit no ready node, per workload |
| `autoscaler.pods.pending.oldest_age` | Gauge, seconds | Age of the oldest pending pod per workload; zero when none is pending |
| `autoscaler.pods.pending.time` | Counter, seconds | Pending pod-seconds: one second for every second each pod waited |
| `autoscaler.pod.pending.duration` | Histogram, seconds | How long a pod waited, recorded once when it is placed; zero when placed at once |
| `autoscaler.workload.replicas.desired` | Gauge | Replicas a workload wants |
| `autoscaler.workload.replicas.available` | Gauge | Replicas placed and started |
| `autoscaler.nodes` | Gauge | Nodes per `group` and lifecycle `state` |
| `autoscaler.group.cpu.capacity` | Gauge, CPU units | CPU of a group's ready nodes |
| `autoscaler.group.cpu.reserved` | Gauge, CPU units | CPU reserved by pods on a group's ready nodes |
| `autoscaler.group.memory.capacity` | Gauge, bytes | Memory of a group's ready nodes |
| `autoscaler.group.memory.reserved` | Gauge, bytes | Memory reserved by pods on a group's ready nodes |
| `autoscaler.cost.rate` | Gauge, USD per hour | Illustrative hourly cost of a group's provisioning, ready and draining nodes |
| `autoscaler.phase` | Gauge | One series per `phase`; the current phase is 1, the others 0 |
| `autoscaler.decisions` | Counter | Each Jev recommendation Reflex processed, by `action` and `outcome` |
| `autoscaler.node.events` | Counter | Node lifecycle steps, by `group` and `event` |
| `autoscaler.node.provision.duration` | Histogram, seconds | Time from a node request to the node being ready |

Every metric carries `policy:jev` and the run's `simulation_run`, for example `autoscaler-1790876343935-3440312-2`. Unlike the scheduler, the run tag is always present, so a run can be found whenever export is on, with or without `--datadog-evidence`. These join the application's service/environment resource tags.

| Tag | Values | On |
|---|---|---|
| `workload` | `web`, `api`, `batch` | Pending pods, pending age, pod wait, replicas |
| `group` | `general_small`, `general_large`, `memory_heavy` | Nodes, group capacity and reservation, cost rate, node events, provisioning time, decisions that name a group |
| `state` | `provisioning`, `ready`, `draining` | `autoscaler.nodes` |
| `phase` | `stable`, `scaling_up`, `scaling_down` | `autoscaler.phase` |
| `event` | `requested`, `ready`, `failed`, `draining`, `removed` | `autoscaler.node.events` |
| `action` | `scale_up`, `remove`, `no_change`, `none` | `autoscaler.decisions`; `none` is an evaluation that produced no action |
| `outcome` | `applied`, `unchanged`, `rejected`, `evaluation_error` | `autoscaler.decisions` |
| `guard` | `fresh`, `within_limits`, `justified_scale_up`, `scale_down_cooldown`, `drainable`, `disruption_budget`, `undefined_transition`, `unknown_node`, `not_offered` | `autoscaler.decisions` with `outcome:rejected` |
| `error` | `true`, `false` | `autoscaler.decisions`; true for `rejected` and `evaluation_error` |

All nine `group`/`state` combinations and all three phases are always reported, so a state that empties reads zero instead of keeping its last value. Memory is converted from model GiB to bytes (1 GiB = 1,073,741,824 bytes). Utilisation is reserved divided by capacity; these are pod requests, not measured usage. Cluster totals are sums across workloads or groups. Node names and pod identities appear only in logs, traces and the playground, never as metric tags.

A rejected recommendation or an evaluation error changes nothing in the cluster and is counted once. `autoscaler.node.events` counts `requested` when a scale-up is accepted, `ready` or `failed` when provisioning ends, `draining` when a removal is accepted, and `removed` when the drain completes. The four starting nodes produce no events.

## Time, pause, and reset

Durations, ages and pending pod-seconds use simulated seconds. Export timestamps and rates derived from counters use wall-clock time, so playback speed affects those rates; with Datadog evidence playback is 1× and the two clocks agree.

Observable gauges keep the current snapshot while paused. Reset, and choosing a scenario, start a new `simulation_run`: the previous run's gauges stop and the new run reports its own starting cluster. Counters and histogram samples are recorded from committed state after each event and are never recounted when state is read or exported.

## Traces and logs

```text
autoscaler.decision
  autoscaler.evaluate
    reflex.evaluate
      typesafe.system_one
        typesafe.http_attempt
  autoscaler.apply
    reflex.execute
```

One decision trace spans the background Jev call, any paused wait, and the guarded execution. Its root span carries `decision_id`, the `phase` and `simulation_time_ms` at evaluation, the `simulation_run`, and the final `status` (`applied`, `unchanged`, `rejected`, `evaluation_error`, or `cancelled` after a reset). Span context never enters model evidence or exports.

The `reflex_sim::autoscaler` target logs each processed recommendation (action, outcome, guard) and each playback or load control, with the run tag.

## Reflex and TypeSafe telemetry

The autoscaler's SDK telemetry goes to the same meter provider as its own metrics and is labelled, so it can be separated from the other simulations:

| Metric | Label |
|---|---|
| `reflex.evaluations` | `controller:cluster_autoscaler` |
| `reflex.transitions` | `machine:cluster_autoscaler` |
| `typesafe.client.requests`, `typesafe.client.request.duration`, `typesafe.client.call.duration`, `typesafe.client.retry.backoff.duration`, `typesafe.client.tokens` | `client:cluster_autoscaler` |

The playground gives each simulation its own named TypeSafe client for this; the other two are `circuit_breaker` and `resource_scheduler`. `reflex.transitions` counts every machine input, including clock ticks and load changes, so it is far larger than the number of scaling actions. `typesafe.client.calls.in_flight` is process-wide and has no client label. With `--datadog` all of these are exported; without it none are.

Example Datadog queries:

```text
max:autoscaler.cpu.requested{service:reflex,env:local,simulation_run:<run>}
max:autoscaler.pods.pending{service:reflex,env:local,simulation_run:<run>} by {workload}
max:autoscaler.nodes{service:reflex,env:local,simulation_run:<run>} by {group,state}
sum:autoscaler.decisions{service:reflex,env:local,simulation_run:<run>} by {action,outcome,guard}.as_count()
sum:typesafe.client.requests{service:reflex,env:local,client:cluster_autoscaler} by {status}.as_count()
```

## Datadog as the autoscaler's evidence source

Add `--datadog-evidence` (with `--features datadog` and `--datadog`) and provide `DD_APP_KEY` with `timeseries_query` permission. Credentials stay in the server process.

The cluster's own state is application-owned and always sent to Jev: nodes, pod placements, pending pods, limits, the revision and the legal actions. Datadog supplies delayed context, attached as `telemetry` when a valid snapshot exists:

```text
telemetry
  source: datadog
  simulation_run, fetched_at_unix_ms, observed_at_unix_ms, age_seconds
  requested_cpu, requested_memory_bytes
  workloads: workload, desired_replicas, available_replicas, pending_pods, oldest_pending_age_seconds
  groups: group, ready_nodes, provisioning_nodes, draining_nodes,
          cpu_capacity, cpu_reserved, memory_capacity_bytes, memory_reserved_bytes
```

All values are maxima from one complete, non-interpolated ten-second bucket; `observed_at_unix_ms` is the start of that bucket and `age_seconds` its age when Jev was asked. Queries are scoped by `env`, `service`, `policy:jev` and the run, exclude the newest twenty seconds to allow ingestion, and require every workload, every group and every group/state series in the same bucket. At most one query is in flight, started at least ten wall-clock seconds apart.

A query result is reused for at most fifteen seconds and an observation is used for at most sixty. Missing, malformed, stale or wrong-run telemetry is simply left out: the evaluation goes ahead on the live cluster, and absent telemetry is never read as no demand or no pending pods. A failed refresh leaves still-valid cached observations in place. In live runs the attached observations were 30–55 seconds old, close to that limit, and the first arrived 65 seconds after play.

In this mode playback is 1× in real time, stepping is disabled, and a run lasts 15 minutes instead of 10. Play, pause and reset clear cached observations and start a new collection boundary; only buckets at least twenty seconds after it are used. Reset also starts a new `simulation_run`. The Surge and Memory-heavy presets apply their changes at twice their local times, so telemetry is available before the first change.

### Forecasts from Datadog history

With a forecast provider attached, Toto's input in this mode is the run's own metrics queried back: `autoscaler.cpu.requested`, `autoscaler.memory.requested` (converted to GiB) and `autoscaler.pods.pending`, summed over the run in ten-second buckets. Local Toto needs 32 complete buckets, so the first forecast arrives a little over six minutes after play (372 seconds in a live run). Its history ends 30–40 seconds before the wall clock, and it predicts twelve ten-second buckets.

The evidence states this provenance in `forecast` (`source: datadog_observations`, `time_domain: forecast_epoch_wall_clock`, `age_ms`, `history_seconds`) and in `forecast_basis` (`source`, `time_domain`, `history_seconds`, `age_seconds`, `max_age_seconds`, `lookahead_seconds`). `forecast_headroom`, which the `justified_scale_up` guard uses, is the p50 rise in requested CPU and memory above the live cluster's current requests within the next 60 seconds, capped at three large nodes.

A Datadog-sourced forecast may justify a scale-up until the last bucket it saw is 60 seconds old. Beyond that it is dropped from the evidence, and if it crosses that age while Jev is deciding, the guard ignores its headroom at execution. A forecast from the simulator's own samples is used for 30 simulated seconds. In both cases a forecast only justifies provisioning: it never counts as usable capacity, and no other guard is relaxed. Removal guards read the live cluster only.

Immediately before applying a recommendation Reflex rechecks the live cluster: the revision, the five-second evidence age, limits, justification, cooldown, drainability and disruption budgets. Datadog observations never authorise a change by themselves.

Import [the autoscaler dashboard](../../dashboards/cluster-autoscaler.json) into a new Datadog dashboard. See [dashboard notes](../../dashboards/README.md) for scope, units and import instructions.
