# Forecasts in the incident playground

Circuit Breaker and Resource Scheduler each maintain their own observation history and can request forecasts from an application-supplied provider. Fresh forecasts are added to the next Jev evaluation. Reflex continues to check current legal choices, freshness, readiness, and resource limits before committing an action.

## Connect a provider

The bundled `LocalToto` adapter calls the optional Python service running the
open-source Toto model on your machine. See [local Toto setup](../../integrations/toto/README.md).

```sh
# Terminal 1, from the repository root:
uv sync --project integrations/toto --python 3.12 --locked
uv run --project integrations/toto --locked reflex-toto

# Terminal 2, with TYPESAFE_API_KEY already set:
cargo run -p reflex-sim --locked -- --playground --policy jev \
  --toto-url http://127.0.0.1:8765
```

To forecast Datadog history, build with `--features datadog` and add
`--datadog --datadog-evidence`. Only the Rust process receives Datadog and
TypeSafe credentials; Python receives numeric observations. Without `--toto-url`,
the CLI starts without a provider and evaluations use observed state only.

Other providers can implement `reflex_sim::capacity::forecast::Forecaster` and
be passed to `serve_with_forecasts`. The interface accepts aligned observations
and returns p10/p50/p90 arrays with origin, interval, provenance, and latency.
`minimum_samples` declares the provider's required observed history on each grid.
The shared driver validates results before use. No provider belongs to the core
Reflex SDK.

## Controls and actuals comparison

Each scenario has a **Toto forecasting** switch. The circuit-breaker switch applies to all three services. With a configured provider, forecasting starts enabled. Turning it off cancels pending forecasts and pending Jev evaluations, omits forecast evidence from subsequent evaluations, and stops Toto calls without resetting traffic or system state. Local observation collection continues. Re-enabling requests a new forecast from available history; the per-run call budget is not reset by toggling. The switch setting survives a simulation reset.

The charts compare actual observations with a frozen forecast selected by its origin time. New forecasts do not overwrite the selected prediction. Up to twelve forecast origins are retained. The green dashed line is the original p50 prediction, the blue line is the subsequently observed mean, and the shaded band averages pointwise p10/p90 predictions. Both series use exactly the same ten-second buckets. An actual point appears only when the entire corresponding bucket has been observed; missing and future values remain gaps, not zeros. Datadog actuals come from subsequent remote history refreshes, so they appear with ingestion and refresh delay. Forecast availability delay is displayed separately from its origin.

## Observation series

| Scenario | Local simulator observations | Datadog observations |
| --- | --- | --- |
| Circuit breaker, one forecast per service | Offered requests/s; failed client responses/s (errors and timeouts); queue depth | `http.client.requests` / bucket seconds (all outcomes); `http.server.queue.depth`; `http.server.utilization` |
| Scheduler | Offered jobs/s; offered CPU-seconds/s; offered GiB-seconds/s | `scheduler.queue.depth`; `scheduler.node.cpu.reserved`; `scheduler.node.memory.reserved` |

Scheduler resource demand uses requested resources and estimated duration of jobs that actually arrived. No future arrivals, actual future completion times, fault settings, fault durations, or seeded prehistory are sent. Datadog mode queries run-scoped, policy-scoped metrics and sums across applicable nodes or clients. Circuit forecasts include completed client request rate across all outcomes, including circuit-blocked requests; this is a delayed proxy for client demand, not an exact arrival-rate measurement. Queue depth and utilization describe server pressure. Missing or misaligned buckets make that refresh unavailable; local data does not substitute for missing Datadog history.

## Timing and uncertainty

Local histories contain up to 256 one-second samples and normally require 64 observed seconds before the first request. The repeating demand preset waits for 180 seconds. Toto predicts the next 120 seconds. Datadog histories contain up to 320 seconds in ten-second buckets and require 320 seconds for local Toto (32 observed samples), plus ingestion delay and the post-resume collection boundary. Their prediction horizon is twelve ten-second buckets. Datadog sessions run for up to ten minutes at 1× speed; local sessions last three simulated minutes, six minutes for scheduler repeating demand, or ten minutes for circuit-breaker cyclical load.

Each forecast driver allows at most 60 refresh attempts per run, no more frequently than every ten data-time seconds and five wall-clock seconds. Datadog warm-up does not spend refresh attempts. RPC work is asynchronous, with a twelve-second total deadline, so simulation and controls remain responsive. Reset cancels pending forecasts and clears history; Datadog pause/resume starts a fresh collection boundary.

Jev receives only the remaining future portion of a forecast, grouped into ten-second buckets. The bucket values are averages of pointwise p10/p50/p90 predictions, **not quantiles of the aggregate bucket total**. Bucket offsets are relative to the forecast origin; `age_ms` gives elapsed time since that origin. Negative projections are clamped to zero for physical demand/pressure summaries. Freshness limits are thirty simulated seconds locally and sixty wall-clock seconds for Datadog, allowing for telemetry ingestion delay. Stale or failed forecasts do not prevent evaluations on valid observed state.

Forecasts describe a continuation of recent observations, not the effect of an untried action. A decline in failures while a breaker blocks traffic is not proof of downstream health. Forecasts cannot make unavailable node capacity legal. Jev receives these limitations in its instructions. No forecast-quality claim is implied by displaying a prediction.

The shared sidecar lives in `reflex-sim::forecasting`, calling the application-owned `Forecaster` interface. The Reflex core SDK remains independent of Toto and Datadog. Jev cost displays still cover reported Jev usage; Toto does not return billing data.

## A predictable workload to explore

With a forecast provider attached, select **Cyclical load · Toto** in **Resource Scheduler** and click **Run**. Three clients repeat a 60-second workload:

- Client 1 sends one job per second throughout.
- Client 2 adds one job per second from +15s to +45s.
- Client 3 adds one job per second from +25s to +35s.

Jobs request two CPU cores and 2, 6, or 4 GiB respectively, with an estimated duration of eight seconds. Peak CPU demand exceeds the pool's 36 cores briefly; average demand stays below capacity so the queue can drain between peaks. Actual durations vary as usual, and Jev's decisions remain unscripted.

Local runs last six minutes and forecasts start after three observed cycles (180 simulated seconds). Local inputs are trailing ten-second means of actual job arrivals and requested CPU/memory demand. Datadog runs last ten minutes and require 320 seconds of observed telemetry plus ingestion delay; forecasts use queue depth and reserved CPU/memory. There is no preloaded history, and Toto receives neither the workload schedule nor future arrivals.

Open **Trends → Actual vs Toto · Placement pressure** and compare a forecast with subsequent observations. Local mode supports 4× playback; Datadog mode uses 1×. Quiet periods make the next peak easier to distinguish from a permanently saturated queue. Forecast accuracy and placement benefits are not guaranteed, and manual workload edits can break the repeating pattern.
