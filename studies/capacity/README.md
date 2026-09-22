# Running the capacity study

The study uses the existing capacity simulator and Reflex executor. It is separate from the interactive playground. Read [PROTOCOL.md](PROTOCOL.md) for workload definitions, tuning, metrics, and limitations.

Build and test:

```sh
cargo test -p reflex-sim --example capacity_study --locked
cargo build -p reflex-sim --example capacity_study --release --locked
```

Supply `TYPESAFE_API_KEY` through the environment for Jev runs; credentials are never written to recordings. Live forecast collection is not bundled. Forecast-enabled arms require existing `forecasts.json` files, one per trace directory (`<pattern>-<load>-<seed>`). The record schema is `ForecastRecord` in [capacity_study.rs](../../crates/reflex-sim/examples/capacity_study.rs); each record contains its input, origin and availability times, snapshot or error, and latency. Raw study recordings are not included in this repository.

```sh
python3 studies/capacity/run_study.py tune --output output/capacity-study-tuning
python3 studies/capacity/run_study.py study --output output/capacity-study \
  --tuning output/capacity-study-tuning/selected.json --seeds 10 --workers 4
```

The `study` command above requires supplied forecast recordings. To run a classical comparison without forecasts or credentials:

```sh
target/release/examples/capacity_study --output output/capacity-classical \
  --seeds 1 --policies fixed2,fixed6,reactive,hpa,ewma,persistence --forecasts none
```

Use a new output directory when changing experimental settings. Completed traces are skipped on resume. Forecast inputs/results and Jev inputs/results are recorded separately. An interrupted trace may contain chargeable recorded API calls even if it has no completed summary; retain those journals for cost accounting before rerunning it. HTTP 401/402/403 writes a shared `STOP.json` and stops all workers. After restoring access, archive interrupted trace outputs and the stop marker before resuming, retaining workloads and forecasts. The cost audit must include archived paid attempts. There are no automatic inference retries. Do not run overlapping workers for the same seed/output directory.

Replay a completed trace without API calls:

```sh
target/release/examples/capacity_study --output output/capacity-study \
  --seeds 1 --first-seed 1001 --patterns spike --loads moderate \
  --forecasts cache --replay --tuning output/capacity-study-tuning/selected.json
```

Replay checks exact evidence and summary equality, re-executing every transition through Reflex. The JSON dependency enables precise floating-point parsing to preserve forecast evidence on round-trip. Replay summaries reproduce recorded inference cost; replay itself incurs no inference cost.

Generate the offline HTML report and standalone scientific plots using Python with `numpy` and `matplotlib` installed:

```sh
python3 studies/capacity/analyze.py output/capacity-study
python3 studies/capacity/supplement.py
python3 studies/capacity/validate.py output/capacity-study --expected-traces 120
python3 studies/capacity/render_paper.py
```

Open `output/capacity-study/report/index.html`. The report includes CSV, summary JSON, paired confidence intervals, resource/SLO plots, forecast diagnostics, and an example timeline. An incomplete input set is clearly labeled interim. Raw workloads, decisions, forecasts, timelines, and job outcomes remain beside the report.

Individual runs and sensitivities can be configured with `target/release/examples/capacity_study --help`. The main timing mode applies every decision after one simulated second. `--timing measured` uses measured RPC latency rounded up to simulation ticks. This study does not publish into Datadog; the event journal is the measurement source.

The standalone paper is `output/capacity-study/report/index.html`; the detailed data view is `explorer.html`. Edit `PAPER.template.md` and run `render_paper.py` to rebuild the paper, generated `PAPER.md`, and download bundle. The renderer requires Markdown in addition to the plotting dependencies. The paper interprets the completed 120-trace dataset; update its prose explicitly when designing a new experiment.

## Efficiency-aware prompt follow-up

The follow-up keeps the original simulator, traces, forecast recordings and model identifier, and changes only the Jev instruction. Read [EFFICIENCY_PROTOCOL.md](EFFICIENCY_PROTOCOL.md) and the frozen [prompt](prompts/efficiency-v1.txt). The original results remain in `output/capacity-study`; new Jev and Jev + Toto outcomes go to `output/capacity-study-efficiency`. The runner's `--prompt-file` option leaves the original default instruction intact.

Run with a funded `TYPESAFE_API_KEY` in the environment; forecast recordings must already exist:

```sh
REFLEX_STUDY_MIN_CALL_MS=400 python3 studies/capacity/run_efficiency.py --workers 6
python3 studies/capacity/run_efficiency.py --replay --workers 2
python3 studies/capacity/analyze_efficiency.py
python3 studies/capacity/validate_efficiency.py
python3 studies/capacity/render_efficiency.py
```

Replay and analysis make no model calls. The alternative `replay_efficiency_when_ready.py` waits for all twelve workload cells of each seed and replays completed seeds while collection continues. Do not run both replay scripts concurrently.

The follow-up report is `output/capacity-study-efficiency/report/index.html`; its manuscript source is `EFFICIENCY_PAPER.template.md`. It reports completed-job queueing delay and arrival-to-completion time (mean, p25, p50, p99), rejected and unfinished jobs, capacity consumption, utilization, scale-down behavior, and inference cost. Main quantiles pool job observations; paired comparisons use equally weighted per-trace statistics. The report includes a preserved copy of the original paper and a downloadable bundle at `output/capacity-study-efficiency-report.zip`.

The efficiency report's charts embed SVG and plain JavaScript, with no external chart library. The first click focuses a series; subsequent clicks add or remove series, and **Show all** restores the comparison. Figures 3 and 4 have a workload selector for traffic, CPU work, or memory work on the right axis. Overlays use actual offered workload in 30-second bins, averaged across the same ten seeds; CPU/memory work uses estimated job duration. PNG figures remain available for print and when JavaScript is disabled. `package_efficiency.py` produces a self-contained paper with interactive figures and static fallbacks, excluding raw recordings and local download links; it does not upload anything.

## Observed-performance feedback follow-up

The feedback experiment keeps the efficiency objective and adds two observed 60-second performance windows plus recent scaling actions to Jev's evidence. See [FEEDBACK_PROTOCOL.md](FEEDBACK_PROTOCOL.md) for cohort/window definitions and the frozen comparison. The new output is `output/capacity-study-feedback`; earlier results remain separate. This feature belongs to the study runner, not the core SDK or interactive playground.

```sh
python3 studies/capacity/run_feedback.py --workers 6 --env-file /path/to/.env.local
python3 studies/capacity/run_feedback.py --workers 2 --replay
output/study-report-venv/bin/python studies/capacity/analyze_feedback.py
output/study-report-venv/bin/python studies/capacity/validate_feedback.py
output/study-report-venv/bin/python studies/capacity/render_feedback.py
```

Alternatively, `replay_feedback_when_ready.py` can replay complete seeds while collection continues; do not run both replay commands concurrently. A completed trace can be checked independently with `validate_feedback.py --trace steady-moderate-1001`. Full validation reconstructs every event-time count, latency distribution, resource integral, and recent-action list from job and lifecycle recordings, in addition to exact replay.

`--env-file` reads only `TYPESAFE_API_KEY` into the process environment and does not execute shell contents. The optional `--performance-feedback` switch leaves old evidence unchanged when absent. Use the same switch and prompt for replay. The report at `output/capacity-study-feedback/report/index.html` compares all 13 arms (1,560 outcomes), reports mean/p25/p50/p99 latency and loss/capacity/cost, and retains multi-series selection and workload overlays. The downloadable report bundle is `output/capacity-study-feedback-report.zip`.
