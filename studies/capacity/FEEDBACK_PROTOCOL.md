# Observed performance feedback in model-guided autoscaling

Frozen before collection. Compare Jev and Jev + Toto with performance feedback against the completed efficiency-prompt experiment and original classical baselines. This is an exploratory follow-up on reused traces, not a new held-out evaluation.

## Intervention

Keep efficiency-v1's objective verbatim; append field definitions and instructions to consider observed outcomes. Add `performance` to the study evidence only (`--performance-feedback`). It contains adjacent trailing 60-second windows, each with observation coverage, offered/completed/rejected counts and rates, completed-job queue-wait and arrival-to-completion mean/p25/p50/p99 plus sample counts, active node-seconds and mean active nodes, CPU and memory reservation utilization. Include applied capacity changes from the last 120 seconds. No cumulative dollar total is sent: recent node-seconds are the comparable resource-efficiency signal.

Windows are (now-60s, now] and (now-120s, now-60s], clipped at simulation start. Before a full window exists, rates use observed seconds; empty latency samples and zero denominators are null. Counts are event-time observations, not arrival cohorts. Rejection fraction is rejections / (completions + rejections), never rejections divided by same-window arrivals. Queue wait and latency include only completions observed within the window and exclude unfinished/rejected jobs. Current queue and oldest wait remain in evidence. Resource usage is integrated over the preceding one-second interval, including ready, starting and draining capacity. Actual duration and scheduled finish time are never read by the feedback collector. Changes and outcome trends are not presented as causal attribution.

## Fixed comparison

120 original paired workload traces: six patterns, moderate/heavy loads, seeds 1001–1010. Two new policy outcomes per trace, 240 total. Same simulator/placement/guards, 300-second warm-up, 1,800-second measurement, up to 120-second completion, Jev model jev-1.13.0, decision every 10 seconds with equalized one-second application delay, same fallback/tuning, same cached Toto forecasts. No new Toto requests. At most six workers; calls spaced at least 400 ms per worker. Ordinary inference errors use recorded fallback; billing/auth failures stop the study. No prompt retuning after viewing results. Existing result directories remain unchanged.

This estimates the behavior of the feedback-plus-field-instruction intervention, not the isolated effect of any individual field. Model stochasticity/service drift remain confounders because prior arms are recorded rather than freshly repeated.

## Measurements and verification

Report completed-job queueing and completion latency mean/p25/p50/p99 in seconds, rejected and unfinished jobs separately, average active node-hours per trace, CPU/memory utilization, starts/drains/rapid direction reversals, inference errors and usage-based estimated cost. Pooled latency distributions are request-weighted. Paired differences use equal trace weights and seed-block bootstrap 95% intervals (10,000 resamples). Compare feedback vs efficiency for each Jev arm and feedback+Toto vs feedback; include classical references without retuning.

Exact replay must reproduce evidence, transitions and summaries for all 120 traces with no inference calls. Verify workload and cached forecast byte identity, outcome conservation, temporal bounds, completed samples and cost from raw journals independently. Inference cost uses the same frozen estimate ($0.042/million input tokens, zero output charge); disclose missing usage, exclude unknown Toto/infrastructure cost, and count any paid preflight or partial attempts separately. Local simulator evidence is used; Datadog is not in this controlled experiment.

## Commands

```sh
cargo test -p reflex-sim --example capacity_study --locked
cargo build -p reflex-sim --example capacity_study --release --locked
python3 studies/capacity/run_feedback.py --workers 6 --env-file /path/to/.env.local
python3 studies/capacity/run_feedback.py --workers 2 --replay
python3 studies/capacity/analyze_feedback.py
python3 studies/capacity/validate_feedback.py
```
