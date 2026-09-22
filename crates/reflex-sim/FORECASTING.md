# Forecasts in the incident playground

Circuit Breaker, Resource Scheduler, and Recovery each maintain their own observation history and can request forecasts from an application-supplied provider. Fresh forecasts are added to the next Jev evaluation. Reflex continues to check current legal choices, freshness, readiness, and resource limits before committing an action.

## Connect a provider

The repository includes forecast types, validation, orchestration, and charts. It does not bundle a live forecasting service adapter. Implement `reflex_sim::capacity::forecast::Forecaster` for your service and pass an `Arc<dyn Forecaster>` to `reflex_sim::playground::serve_with_forecasts`.

The trait accepts an `Input` containing aligned observations and a prediction length. Return a `Snapshot` containing p10/p50/p90 arrays, the origin, interval, source, model provenance, and request latency. The driver validates the snapshot against the input before using it. Authentication and service transport belong in your adapter. See [the interface](src/capacity/forecast.rs) and [mock-provider tests](src/forecasting.rs) for the contract.

The standard CLI starts without a forecast provider:

```sh
cargo run -p reflex-sim --locked -- --playground --policy jev --port 8742
```

`TYPESAFE_API_KEY` stays on the server. The playground reports that forecasting is not configured. With a provider attached, the three tabs show forecast status, p50 lines, p10–p90 bands, history length, source, age, and inspectable evidence. Selecting a circuit-breaker service selects its forecast. Deterministic policies do not use forecast evidence. The current UI labels refer to Toto, the forecaster used in the research study.

For Datadog state and telemetry export:

```sh
cargo run -p reflex-sim --locked --example datadog_playground -- \
  --datadog-evidence --duration-secs 600 --port 8743
```

This also requires `DD_API_KEY` and `DD_APP_KEY` with metrics read permissions; use the existing `DD_SITE`, `DD_SERVICE`, and `DD_ENV` settings. No credentials are sent to the browser. This runner also starts without a forecast provider.

## Controls and actuals comparison

Each scenario has a **Toto forecasting** switch. The circuit-breaker switch applies to all three services. With a configured provider, forecasting starts enabled. Turning it off cancels pending forecasts and pending Jev evaluations, omits forecast evidence from subsequent evaluations, and stops Toto calls without resetting traffic or system state. Local observation collection continues. Re-enabling requests a new forecast from available history; the per-run call budget is not reset by toggling. The switch setting survives a simulation reset.

The charts compare actual observations with a frozen forecast selected by its origin time. New forecasts do not overwrite the selected prediction. Up to twelve forecast origins are retained. The green dashed line is the original p50 prediction, the blue line is the subsequently observed mean, and the shaded band averages pointwise p10/p90 predictions. Both series use exactly the same ten-second buckets. An actual point appears only when the entire corresponding bucket has been observed; missing and future values remain gaps, not zeros. Datadog actuals come from subsequent remote history refreshes, so they appear with ingestion and refresh delay. Forecast availability delay is displayed separately from its origin.

## Observation series

| Scenario | Local simulator observations | Datadog observations |
| --- | --- | --- |
| Circuit breaker, one forecast per service | Offered requests/s; failed client responses/s (errors and timeouts); queue depth | `http.server.queue.depth`; `http.server.active`; `http.server.utilization` |
| Scheduler | Offered jobs/s; offered CPU-seconds/s; offered GiB-seconds/s | `scheduler.queue.depth`; `scheduler.node.cpu.reserved`; `scheduler.node.memory.reserved` |
| Recovery | Offered requests/s; failed final responses/s excluding rejected traffic; aggregate replica queue depth | `http.server.queue.depth`; `http.server.active`; `recovery.replica.requests.outstanding` |

Scheduler resource demand uses requested resources and estimated duration of jobs that actually arrived. No future arrivals, actual future completion times, fault settings, fault durations, or seeded prehistory are sent. Datadog mode queries run-scoped, policy-scoped, service-scoped gauges and sums across applicable nodes, clients, or replicas. It forecasts observed pressure, rather than inventing offered demand from completed requests. Missing or misaligned buckets make that refresh unavailable; local data does not substitute for missing Datadog history.

## Timing and uncertainty

Local histories contain up to 256 one-second samples and normally require 64 observed seconds before the first request. The repeating demand preset waits for 180 seconds. Toto predicts the next 120 seconds. Datadog histories contain up to 320 seconds in ten-second buckets and require 160 seconds, plus ingestion delay and the post-resume collection boundary. Their prediction horizon is twelve ten-second buckets. Datadog sessions run for up to ten minutes at 1× speed; local sessions last three simulated minutes, or six minutes for repeating demand.

Each forecast driver allows at most 60 refresh attempts per run, no more frequently than every ten data-time seconds and five wall-clock seconds. Datadog warm-up does not spend refresh attempts. RPC work is asynchronous, with a twelve-second total deadline, so simulation and controls remain responsive. Reset cancels pending forecasts and clears history; Datadog pause/resume starts a fresh collection boundary.

Jev receives only the remaining future portion of a forecast, grouped into ten-second buckets. The bucket values are averages of pointwise p10/p50/p90 predictions, **not quantiles of the aggregate bucket total**. Bucket offsets are relative to the forecast origin; `age_ms` gives elapsed time since that origin. Negative projections are clamped to zero for physical demand/pressure summaries. Freshness limits are thirty simulated seconds locally and sixty wall-clock seconds for Datadog, allowing for telemetry ingestion delay. Stale or failed forecasts do not prevent evaluations on valid observed state.

Forecasts describe a continuation of recent observations, not the effect of an untried action. A decline in failures while a breaker blocks traffic is not proof of downstream health. Forecasts cannot establish replica readiness, authorize additional retries, or make unavailable node capacity legal. Jev receives these limitations in its instructions. No forecast-quality claim is implied by displaying a prediction.

The shared sidecar lives in `reflex-sim::forecasting`, calling the application-owned `Forecaster` interface. The Reflex core SDK remains independent of Toto and Datadog. Jev cost displays still cover reported Jev usage; Toto does not return billing data.

## A predictable workload to explore

With a forecast provider attached, in **Resource Scheduler**, select **Repeating demand waves · Toto example** and leave Toto forecasting enabled. Each of three clients follows the same smooth 60-second demand cycle, updated every five seconds, with fixed request sizes and rates from 0.08 to 0.28 jobs/s per client. The local run lasts six minutes. Use 4× playback to shorten the wait.

This preset waits for three observed cycles (180 simulated seconds) before requesting its first forecast. Its local inputs are trailing ten-second means of actual arrived jobs and their requested resource demand, sampled every second. The series names explicitly include `10s_mean`. Toto sees only the observations; it receives neither the cycle formula nor future arrivals. Datadog mode continues to forecast the remote pressure metrics listed above, with its usual history requirements.

Select a forecast origin of 180s or later and let the next 120 seconds run. The blue observations should reveal how closely the original green prediction follows the repeated peaks and troughs. Both plotted lines average the same ten-second buckets of the smoothed series. Manual rate or size changes can break this regularity.

A live check at origin 180s compared the following twelve ten-second buckets with a persistence baseline that holds the final observed value constant. Toto's mean absolute error for job arrival rate was 0.0234 jobs/s versus 0.2425 jobs/s for persistence, about 90% lower. This is one origin of one deliberately predictable synthetic workload, not evidence that forecasting improves Jev's placement decisions. The same resource profiles make the three demand series proportional; their scores are not independent experiments.
