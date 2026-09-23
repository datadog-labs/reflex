# Local Toto forecasting

This Python package runs the open-source [Toto 2.0](https://github.com/DataDog/toto)
model on your machine. The simulator sends observed time series to its loopback
HTTP API and adds fresh forecasts to Jev's state. Reflex still checks each proposed
action against current guards.

## Run

From the repository root, start the forecasting service:

```sh
uv sync --project integrations/toto --python 3.12 --locked
uv run --project integrations/toto --locked reflex-toto
```

Wait for `Ready: http://127.0.0.1:8765`. The first start downloads the public
22-million-parameter checkpoint (about 88 MB of weights) into the Hugging Face
cache. No account, Datadog infrastructure, or API key is needed for Toto.
Subsequent inference runs locally; set `HF_HUB_OFFLINE=1` to start from cached
files without contacting Hugging Face. The service loads and warms the model
once. It uses CPU by default; `--device cuda` selects an available CUDA GPU.

In another terminal, start the playground with your TypeSafe key already set:

```sh
cargo run -p reflex-sim --locked -- \
  --playground --policy jev --toto-url http://127.0.0.1:8765
```

To forecast queried Datadog telemetry instead of local observations, add
`--features datadog` before `--` and `--datadog --datadog-evidence` after it.
See [Datadog setup](../../crates/reflex-sim/DATADOG.md) for the required keys.
The Rust process owns those credentials. Python receives only numeric history.

All three simulations use the same service. Their **Toto forecasting** switches
stop forecast requests and remove forecast evidence from subsequent evaluations.
The charts compare a frozen prediction with actual observations as they arrive.
In Circuit Breaker, select a service to see its forecast in the inspector.

For a first run, choose **Resource Scheduler → Repeating demand waves · Toto
example**, then run at 4× speed. It collects three cycles (180 simulated seconds)
before forecasting. Ordinary local scenarios need 64 seconds; Datadog mode needs
32 complete ten-second buckets (320 seconds), plus ingestion delay. Every forecast
covers the next 120 seconds: 120 local steps or 12 Datadog steps. Deterministic
policies can display forecasts, but only Jev consumes them as decision evidence.

## Model and transport

The default checkpoint is `Datadog/Toto-2.0-22m`, pinned to revision
`685e4ae3e2be8d8998025e53dd98e7fdcb296a89`. Python dependencies are locked in
`uv.lock`. Override the model with `--model` and optionally `--revision`; an
unpinned override resolves `main` at startup and reports the resolved commit.
Use `--threads` to change CPU parallelism (default 4). Model weights are not
included in this repository or Python package.

The adapter preserves history order, forecast origin, and sampling interval.
Toto consumes `[batch, series, time]`; the HTTP API uses `[time, series]`.
Non-multiple-of-32 histories are padded on the left and masked as missing. No
padding is counted as observed history, and the final observed sample stays at
the forecast origin. At least 32 actual observations are required because Toto
marks its final context patch as observed. Forecasts use single-pass decoding
and return p10/p50/p90 without clamping or sorting them. Both service and simulator
reject invalid dimensions, nonfinite values, and crossing quantiles.

The service accepts one inference at a time and at most four waiting requests.
A full queue returns HTTP 429; a queue wait exceeding eight seconds returns 503.
The Rust HTTP deadline is ten seconds, within the playground's twelve-second
forecast deadline. Disconnecting does not interrupt a PyTorch forward pass;
its slot remains occupied until inference finishes. Reset/off discards pending
Rust results. Failed or stale forecasts leave Jev using valid observed state;
missing Datadog state is still an evaluation error.

This is a local development service, bound to `127.0.0.1`, without authentication.
Do not expose it through a public proxy. The Rust client accepts only loopback
URLs, disables proxies and redirects, and bounds response size. Bodies and
Datadog/TypeSafe keys are not logged. `/health` remains available during inference.

## HTTP contract

`GET /health` returns readiness, model provenance, and whether inference is busy.
Readiness is advertised only after model loading and a warmup forward pass.

`POST /forecast` accepts the existing Rust
[`Input`](../../crates/reflex-sim/src/capacity/forecast.rs) as JSON:

- `origin_ms`: time of the final observed sample in the simulator's time domain.
- `timestamps`: regular Unix-second timestamps ending at `1780000000 + origin_ms / 1000`.
- `values`: one row per timestamp, three finite values per row, at most 8192 rows.
- `interval_ms`: 1000 (default) or 10000.
- `prediction_length`: future steps, from 1 to 120.

The response is a Rust `Snapshot`: matching origin and interval, request ID,
`source: "local_toto"`, resolved model provenance, `[0.1, 0.5, 0.9]` quantiles,
three series of `lower`/`median`/`upper` arrays, and total request latency in ms.
The first forecast value is one interval after the origin. Series retain input
column order; their metric names remain with the simulator driver.

## Test

```sh
uv run --project integrations/toto --locked python -m unittest discover -s integrations/toto/tests -v
cargo test -p reflex-sim --test toto --locked
```

With the default service running, exercise real inference through Rust:

```sh
cargo test -p reflex-sim --test toto --locked \
  live_local_toto_forecasts_both_grids -- --ignored --nocapture
```

Tests use synthetic observations and do not call Jev or Datadog. A predictable
workload makes forecast errors easy to inspect; it does not establish that
forecasting improves control decisions.
