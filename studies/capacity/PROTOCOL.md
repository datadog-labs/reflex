# Capacity policy study: protocol and analysis notes

The workload, policy and tuning configuration was frozen before held-out evaluation; its original snapshot is retained with source hashes. Analysis clarifications and additional diagnostics are recorded below.

This is a synthetic capacity-management study, not a Kubernetes or job-placement benchmark. The runner uses the existing Reflex capacity state machine, resource guards and FIFO first-fit placement. It does not change the playground.

## Environment and workloads

Six nodes (8 CPU, 16 GiB), two initially ready, 60-second startup, no preemption/migration, strict FIFO placement, 128 queued jobs maximum, 60-second queue expiry, one-second simulation ticks. Nodes reserve CPU/memory until completion; utilization is reservation utilization, not measured CPU activity. Retain one ready node and at most six active nodes, including starting/draining. Capacity action choices: hold, start one/two, drain one. Ten-second change cooldown applies to all controllers.

Each trace has 256 seconds of prehistory, 300 seconds of executed warm-up, 1,800 seconds of measured arrivals, then at most 120 seconds to complete remaining work. Policies stop accruing resources when their queues/running jobs empty after arrivals stop. Warm-up requests are excluded from service outcomes; all inference calls, including warm-up, are charged and recorded. Resource charges cover the measurement and completion periods. Fixed-six reaches six during warm-up; no policy starts with privileged capacity during measurement.

Independent seeded Poisson arrivals for three client classes: rates 0.35/0.16/0.14 per second, requested CPU 1/4/1, memory 1/2/8 GiB, estimated durations 6/12/18 seconds. Actual durations are ceil(estimate × Uniform[0.75,1.25]); only the simulator knows them. Moderate load multiplies rates by one, heavy by 1.5. Each generator uses disjoint deterministic per-time/per-client random draws, shared across policies.

Workload patterns (times relative to measurement start): steady; surprise step from 0.8× to 2.5× during seconds 600–900; ramp from 0.7× to 2.2× during 300–900, plateau until 1200, decline until 1650; sinusoidal waves 0.6×–2.0× with period 300s; 3× bursts for 40s every 240s against 0.7× background; resource mix swaps CPU and memory requests at 600–1200s (still within node bounds). Fixed pattern schedules are never exposed to models. Random seeds vary arrivals and job durations, not pattern timing. The synthetic patterns are not evidence of production generality.

## Policies and frozen tuning

Fixed-two and fixed-six reference points; observed-demand reactive; utilization-ratio HPA-inspired (not actual Kubernetes HPA); EWMA demand; persistence; Toto+reactive; Jev; Jev+Toto. HPA includes tolerance and stabilized downscale recommendations plus explicit queue rescue; therefore it is not a literal HPA implementation. Persistence equals the reactive controller mathematically under its max(observed,forecast) rule and is included as an implementation check.

Tuning uses seeds 11–13 on all six patterns and both loads. Grid: target utilization 0.6/0.8/0.9; downscale wait 30/120s; EWMA alpha 0.2/0.6. For each classical family independently, select minimum mean node-seconds among configurations with mean SLO success ≥99%; if none qualifies, maximize SLO success then minimize resources. Freeze selected parameters. Toto+reactive and persistence use the reactive settings unchanged, isolating the forecast effect. This does not optimize a forecast-specific controller. Jev's one fixed prompt is recorded verbatim in manifests; no outcome-driven prompt search.

Held-out initial study: seeds 1001–1010, six patterns, two loads, nine policies = 1,080 policy runs on 120 paired traces. This is an initial study with ten independent seeds per workload cell, not a claim of sufficient statistical power for every effect. A larger sample and repeated live model draws can follow if warranted; do not treat thousands of correlated requests as independent replicates.

## Forecasts, evidence, and timing

Toto receives the preceding 256 seconds of offered demand only, every 20 simulated seconds, with 120-second horizon and pointwise p10/p50/p90. Predictions are obtained once per trace and replayed identically across its forecast policies. Responses/errors, inputs, measured RPC time, request IDs, and configured model provenance are recorded. The service does not return a verified forecast model version. Current input timestamps use the existing simulator's fixed epoch; periodicity/time-of-day claims are out of scope.

Both Jev arms receive identical kinds of current state and a 256-second history summarized into 16-second means. Only the Toto arm additionally receives ten-second forecast summaries. Actual futures, scenario names, seeds, and actual job durations are not evidence. Toto sees finer history than Jev; this is a stated limitation of the feature pipeline comparison.

Primary equal-timing mode assigns every policy action and forecast a one-second availability delay. Traffic advances before execution; Reflex rechecks freshness/revision and may reject proposals. Measured-timing sensitivity uses ceil(measured inference latency / 1s), minimum one tick, including failures. The one-second grid cannot resolve subsecond latency differences. No inference retries; failures and missing usage are reported, with reactive fallback for failed Jev evaluations. Missing forecasts are visible and never fabricated. No hidden future-based oracle in primary results.

## Outcomes and cost

Primary SLO: job completed within estimated duration + 20 seconds of arrival. Denominator is every offered measured-period job, including drops/expirations. Secondary: completion/rejection/unfinished counts, p50/p95/p99 wait among completed jobs, p95 completion latency, active node-seconds, reservation utilization, scaling changes, rejected guards, fallback counts and decision latency. Record full per-job data and timelines so additional metrics can be computed without inference.

Jev usage is taken from responses, priced only for resolved model jev-1.13.0 at $0.042 per million input tokens; output free. Source https://docs.typesafe.ai/models checked 2026-09-20. Unknown-price and missing-usage calls stay visible and are excluded from the known estimate. Toto monetary cost is unknown, never zero. Illustrative node price $0.10/node-hour is an explicit scenario assumption, not a cloud quote. Report resource/inference costs separately and the known subtotal. Model costs include warm-up calls; node costs exclude warm-up.

## Analysis and reproducibility

Use per-trace paired differences, macro averages across workload cells and seeds, and paired seed-cluster bootstrap 95% confidence intervals (resample whole seeds, carrying all workload cells and policies together, because the same seed shares random draws across cells). Report workload-specific results, not only an overall winner. Inspect failures and guard counts. Report seed-level variability; a single model execution per trace does not separately estimate model stochasticity. Exploratory comparisons are not multiplicity-corrected confirmatory hypothesis tests.

Save manifests, raw decision evidence/responses, token usage, immutable workload traces, Toto forecasts, timelines, job outcomes, summary JSON and tabular reports. Verify exact no-network replay before interpreting results. Development, preflight and measured-mode costs are separately attributable from primary-study cost. Do not silently replace failed model arms with classical results.

## Additional diagnostics

A measured-latency sensitivity uses the first held-out seed on each moderate workload, comparing HPA-inspired, Jev, and Jev+Toto with new live Jev calls and the same recorded forecasts. It includes independent model-response variation and is not a pure causal latency ablation. Main confidence intervals use whole-seed blocks because seeds share draws across workload cells; this clarifies the dependence structure rather than assuming cells are independent.

A known-future diagnostic supplies the actual next 120 seconds of offered demand to the forecast-enabled reactive policy on the same six moderate traces. It is explicitly non-deployable and not an optimal-controller bound. Execution workers may use extra wall-clock request spacing; this does not advance the equal-timing simulated clock. OS file locks prevent duplicate paid seed execution.

Jev has a five-second inference deadline; Toto has an eight-second RPC deadline. The fixed 256-second forecast context is shorter than the 300-second wave period. One-second work series are sparse; pointwise median forecasts and their time averages are not aggregate expected demand. These are limitations of the current evidence representation, not claims about the best attainable Toto performance.

## Interrupted-run audit

The initial account exhausted its credits during held-out evaluation. The first runner version continued with the documented reactive fallback on HTTP 402; those runs do not measure Jev. All affected or incomplete trace attempts were quarantined, preserving their paid usage, and rerun with a funded key. Completed traces without billing failures were retained. The updated runner stops all workers on HTTP 401/402/403, and validation rejects any accepted trace containing those failures. Workloads, cached forecasts, model name, prompt, policy parameters and simulated timing stayed unchanged across the key replacement. API billing-attempt counts and unknown usage are reported separately from accepted-study results.
