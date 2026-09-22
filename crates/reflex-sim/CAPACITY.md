> Historical capacity experiment design. The `/capacity` UI and `--capacity-replay` CLI option are retired. The capacity module and study recordings remain available to the research runner. For the current three-tab playground, see [Forecasting](FORECASTING.md).

# Forecast capacity playground

The retired capacity playground compared three independent systems receiving identical offered requests:

- **Reactive thresholds** scales from the last ten seconds of observed demand and queue pressure.
- **Toto + thresholds** adds forecast demand to the same deterministic policy.
- **Toto + Jev** asks Jev to choose a capacity action using current observations and the same available forecast.

Each system uses the Reflex executor. Placement is identical FIFO first-fit in all three systems, so the comparison isolates capacity decisions. Select a comparison card to inspect its topology. Colored blocks show running jobs and queued work; CPU and memory counters show actual reservations.

## Forecasting dependency

The core Reflex SDK has no forecasting dependency. The capacity module accepts an application-owned `Forecaster`; no live adapter is bundled. The [research runner](../../studies/capacity/README.md) supports supplied forecast recordings, and the [current playground](FORECASTING.md) accepts an injected provider.

## Simulation

Runs last 600 simulated seconds, with a one-second event grid and seeded request arrivals. One to eight clients have independent base arrival rates, CPU/memory sizes, and estimated job durations. Actual durations vary deterministically around estimates. Only estimates enter demand histories; actual future completion times remain simulator-private.

Three patterns exercise different forecasting conditions: **Recurring waves**, **Sustained ramp**, and **Unannounced burst**. Changing patterns resets the run. Initial context is 256 seconds of synthetic prehistory from the chosen pattern. The pattern name and future event schedule are never model inputs. Client edits change subsequent arrivals in all lanes. Offered traffic is measured before admission, so dropped work cannot hide demand from the forecaster.

Six nodes each provide 8 CPU and 16 GiB. Two start ready. Their lifecycle is:

```text
Off → Starting → Ready → Draining → Off
```

Startup defaults to 60 seconds and is configurable from 5–120 seconds. Starting nodes reserve budget but cannot accept jobs. Draining nodes finish current jobs and accept no new ones. At least one node stays ready. Requests larger than a node are rejected. FIFO queues hold at most 128 jobs; queued jobs expire after 60 seconds. Runs stop without draining outstanding work, so compare completed, queued, running and rejected counts together.

## Forecast and decision evidence

Every request to Toto contains a 256×3 time-major matrix on a one-second grid:

1. Offered jobs per second.
2. Offered CPU-seconds per second, using requested CPU × estimated duration.
3. Offered GiB-seconds per second, using requested memory × estimated duration.

The horizon is 120 seconds. The forecasting interface requires aligned p10/p50/p90 arrays and validates lengths, finite numbers, and ordered bounds. Future timestamps follow from the request origin and interval. These are pointwise bounds, not a calibrated coverage guarantee or a joint bound on total future demand. Raw negative predictions remain in the recording; physical-demand summaries clamp them to zero.

Jev receives current queue depth and age, running counts, ready/starting/draining nodes, actual resource availability, startup ETAs, budget, recent change age, ten-second observed demand, legal choices, and the fresh forecast. Forecast evidence uses ten-second buckets containing the mean of each pointwise quantile, with explicit time ranges, origin, age and provenance. Those means are not quantiles of an aggregated random variable. Jev selects **hold**, **start one**, **start two**, or **drain one**. Its confidence and choice probabilities remain available in the decision inspector. When only one action is legal, the executor applies that action directly, labels it deterministic, and makes no Jev call.

The threshold policy targets 80% utilization using observed demand, plus the maximum ten-second mean p90 CPU/memory demand when a forecast is available. It counts starting capacity, responds to queues, and waits at least 60 seconds after a change before scaling down. This is a reproducible reference policy, not an optimized autoscaler.

Before committing a proposal, Reflex rechecks evidence age (10 seconds), capacity/configuration revision, forecast age (30 seconds when used), ten-second change cooldown, node budget, draining eligibility, and resource invariants. Forecasted capacity never counts as usable capacity. Model confidence does not bypass these checks.

## Async behavior and fallback

Toto dispatches no faster than every ten simulated seconds and every five wall-clock seconds, with one request in flight, an eight-second deadline, and a 60-call run limit. Jev dispatches no faster than every ten simulated seconds and every wall-clock second, with one request in flight and a two-second deadline. `--jev-max-evaluations` bounds its calls per run.

Traffic continues while inference runs. At high playback speeds, a response can already be stale when received. Missing/stale forecasts, missing Jev credentials, exhausted Jev budgets and evaluation failures produce labeled reactive fallback. Invalid forecasts never replace the last valid forecast. Guard rejection keeps actual capacity unchanged and appears in the journal.

Pause freezes simulated time and application of pending responses. Already-dispatched calls may finish; Jev usage is still counted. Reset cancels pending tasks. Cost survives reset within this capacity server session; unreported or unknown-price usage is explicitly excluded from the estimate. Toto call counts and successful-response latency are tracked, but no monetary price is assumed.

## Compare, export and replay

Cards compare completions, rejection counts, mean wait among started jobs and active node-seconds. Starting and draining nodes count as active. The chart shows offered CPU work (ten-second trailing average), selected ready CPU capacity, and the latest forecast median/bounds. The MAE and empirical interval coverage compare realized CPU work with the latest forecast available before that second; they are descriptive, not proof of calibration.

**Replay this run** re-applies the exact ordered traffic, client/configuration changes, forecast availability and decision inputs. It rechecks execution outcomes and stops on divergence. It makes no Toto or Jev calls and does not re-add inference cost. In replay, call counters describe recorded responses/decisions, not new calls. An unfinished call at export has no response to replay.

The historical export saved the recording and current summary. The capacity module validates recordings before loading them; the retired CLI replay option is no longer available. Results are reproducible for a recorded run; live inference timing and stochastic model decisions can differ between fresh runs with the same seed.

## Validate

```sh
cargo test --workspace --all-features --locked
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo fmt --all -- --check
```
