# Circuit-breaker dashboard

Import [circuit-breaker.json](circuit-breaker.json) into a **new blank Datadog dashboard** using Configure → Import dashboard JSON. Import replaces the current dashboard contents. See [Datadog’s import instructions](https://docs.datadoghq.com/dashboards/configure/).

## Filters and time range

- `env`: defaults to `local`.
- `policy`: defaults to `jev`.
- `run`: select the `simulation_run` for the incident. `*` combines tagged runs; select one run for unambiguous state and load charts. HTTP and breaker panels require run-tagged metrics from `--datadog-evidence`; older local-evidence runs without that tag are outside their scope.
- `application`: defaults to `reflex`, and applies only to the Jev/Reflex group.

Select the time range covering your chosen run. Allow for telemetry ingestion delay before checking the charts.

## Reading the charts

Client and breaker metrics use `upstream:catalog|payments|search`. Server metrics use `service:catalog|payments|search`. Do not apply `service:reflex` to server metrics.

Client counts record terminal outcomes, including blocked requests. Server counts record completions or queue rejection. A client timeout can be followed by a late server success. Client failure percentages and latency exclude locally blocked requests. A zero denominator or missing series may show no value; the dashboard does not synthesize healthy data from missing telemetry.

Each service has three one-hot state series: green closed, orange probe, red open. The current state has value 1. Datadog rolls up gauge samples, so zoom into the incident for short probe transitions; the transition counter also records changes between gauge snapshots. State and load charts use maxima across matching series; combining multiple runs can show several states active at once.

Rates are completions per wall-clock second. HTTP duration and queue-wait histograms measure simulated seconds, displayed as milliseconds. These clocks align during 1× playback. Count charts show increments per displayed bucket; summary counts cover the selected time range. Enable percentile aggregations for the duration distributions if p95 charts are empty.

The last group uses SDK metrics, which lack run/policy tags, so it ignores the run and policy filters. Its TypeSafe attempt, latency and token panels are scoped to `client:circuit_breaker` and its Reflex evaluations panel to `controller:circuit_breaker`. These panels show nothing for runs exported before the `client` and `controller` labels existed, because those runs' metrics have neither tag. The calls-in-flight panel is a process-wide gauge with no `client` tag and includes scheduler and autoscaler activity. Token usage is not dollar cost. No credentials are included in this dashboard.

## Validation

The JSON was type-checked with the official Datadog Python API client dashboard model, and its metric queries were checked through the timeseries API. Dashboard rendering and import have not been verified in the UI.

## Resource scheduler

Import [resource-scheduler.json](resource-scheduler.json) into a new blank dashboard using Configure → Import dashboard JSON.

The dashboard covers completed/rejected jobs, queue depths and oldest ages, p95 waiting and execution times, per-node CPU/memory reservations and remaining capacity, running jobs, incoming job sizes, and placement outcomes. CPU and memory reservation percentages do not measure actual utilization. Placement attempts can repeat for a queued job; they are distinct from terminal job counts.

Filters are `env` (default `local`), `application` (`reflex`), `policy` (`jev`), and `run` (`*`). Select one scheduler `simulation_run` from Datadog-evidence mode for unambiguous queue/capacity interpretation. The SDK section ignores run and policy: Reflex metrics are scoped to `controller:resource_scheduler` and `machine:resource_scheduler`, and TypeSafe latency and tokens to `client:resource_scheduler`. The two TypeSafe panels show nothing for runs exported before the `client` label existed, because those runs' metrics have no `client` tag. Percentile charts require distribution percentiles enabled. A never-emitted outcome, such as deferral when every attempt placed a job, can show No Data rather than zero.

Set the time range to cover your selected run and allow for telemetry ingestion delay. The dashboard JSON and metric queries were validated through the Datadog API.

## Cluster autoscaler

Import [cluster-autoscaler.json](cluster-autoscaler.json) into a new blank dashboard using Configure → Import dashboard JSON.

The dashboard covers requested and ready CPU and memory, pending pods and their oldest age, pending pod-seconds and pod wait p95, replicas wanted and available per workload, nodes by group and lifecycle state, CPU and memory reservation per group, node cost rate, provisioning time and lifecycle events, the autoscaler phase, and Jev's recommendations by action, outcome and rejecting guard. CPU and memory figures are pod requests and reservations, not measured utilization. A recommendation that Reflex rejects is counted as a recommendation, not as a change to the cluster.

Filters are `env` (default `local`), `application` (`reflex`), `policy` (`jev`), and `run` (`*`). Every autoscaler metric carries `simulation_run` whenever `--datadog` export is on, with or without `--datadog-evidence`; select one run for unambiguous demand, capacity and node counts. The SDK section ignores run and policy but is scoped to the autoscaler: Reflex metrics by `controller:cluster_autoscaler` and `machine:cluster_autoscaler`, TypeSafe latency, requests and tokens by `client:cluster_autoscaler`. Percentile charts require distribution percentiles enabled. An outcome that never occurred, such as a provisioning failure, can show No Data rather than zero.

Set the time range to cover your selected run and allow for telemetry ingestion delay. The metric queries were checked through the Datadog timeseries API against a live run. The dashboard JSON has not been imported or rendered in the UI.
