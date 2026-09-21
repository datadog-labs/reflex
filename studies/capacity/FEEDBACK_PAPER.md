# Can Recent Performance Feedback Improve Model-Guided Autoscaling?

<p class="subtitle">A paired follow-up with observed latency, rejection, utilization, and scaling history</p>
<p class="byline">Technical report · September 2026 · Exploratory simulation study</p>

## Abstract

An autoscaling controller chooses how much capacity to keep available as demand changes. A model can see current queue pressure without knowing whether recent operation has delivered acceptable service. We test whether adding explicit performance feedback changes Jev's balance between delay and resource consumption. Two new arms—Jev and Jev with Toto forecasts—receive adjacent 60-second performance windows and recent scaling actions. The efficiency objective, simulator, workloads, model identifier, action guards, and recorded forecasts remain fixed. We run each arm on 120 paired traces and compare 240 new outcomes with 1,320 prior outcomes. Jev · feedback used 2.333 active node-hours per trace (-2.9% versus Jev · efficiency), with pooled mean queue wait 23.15 versus 21.58 seconds and rejection 3.26% versus 2.93%. Jev + Toto · feedback used 2.251 active node-hours per trace (-2.1% versus Jev + Toto · efficiency), with pooled mean queue wait 24.66 versus 23.49 seconds and rejection 3.37% versus 3.27%. The intervention combines additional evidence with instructions explaining that evidence; this is an exploratory comparison on previously examined workloads, not a held-out causal demonstration.

## 1. Research question

More worker nodes can shorten a queue, but idle, starting, and draining nodes still consume resources. An autoscaler must decide when to accept the startup penalty of adding capacity and when to reclaim capacity that is no longer useful.

Our initial Jev instruction prioritized a completion deadline and generally retained near-maximum capacity. An efficiency-aware follow-up encouraged scale-down. That reduced node-hours but increased delay, rejection, and rapid reversals between scaling directions. Neither experiment supplied rolling distributions of observed latency or rejection. Jev saw queue depth, oldest wait, demand, node resources, and lifecycle state, but had to infer recent service quality from those signals.

This experiment asks: **does explicit recent performance feedback improve that tradeoff under the same efficiency objective?** We measure observed behavior rather than presuming that more context must help. We do not define a new composite score or tune the instruction after viewing these results.

## 2. System and methods

### 2.1 Execution environment

Reflex is the Rust state-machine library that checks and applies proposed actions. Jev is the TypeSafe AI decision model used in this experiment. It selects among hold, start one node, start two nodes, and drain one node. A node can be off, starting, ready, or draining. Startup takes 60 seconds; a draining node finishes existing work and receives no new assignments. There are at most six nodes, each with eight CPU units and 16 GiB memory, and two nodes are initially ready.

The executor checks current legality, observation freshness, lifecycle revision, a ten-second cooldown, and resource invariants. At least one ready node must remain. A legal drain requires an empty queue and no drain already in progress. These checks preserve the implemented constraints; they do not guarantee a good capacity decision.

Jobs reserve CPU and memory until completion. Placement is strict FIFO first-fit for every policy. The queue holds at most 128 jobs; queued jobs expire after 60 seconds. There is no preemption, migration, CPU sharing, or contention-dependent execution slowdown. This experiment compares **autoscaling policies**, not placement algorithms. It uses local simulator observations; Datadog queries are not part of the measurement path.

### 2.2 Workloads and pairing

Three independent seeded Poisson arrival streams represent small, CPU-heavy, and memory-heavy jobs:

| Class | Moderate jobs/s | CPU | Memory (GiB) | Estimated duration (s) |
|---|---:|---:|---:|---:|
| Small | 0.35 | 1 | 1 | 6 |
| CPU-heavy | 0.16 | 4 | 2 | 12 |
| Memory-heavy | 0.14 | 1 | 8 | 18 |

Actual duration is the ceiling of the estimate multiplied by a seeded uniform draw from 0.75 to 1.25. That duration is hidden from the policy until its effects become observable. Heavy load multiplies all arrival rates by 1.5. Six patterns modify demand during the measured period:

| Pattern | Definition |
|---|---|
| Steady | Arrival-rate multiplier 1.0. |
| Spike | Multiplier 0.8, increasing to 2.5 between seconds 600 and 900. |
| Ramp | 0.7 until second 300; linear increase to 2.2 at 900; hold until 1200; decline to 0.7 at 1650. |
| Waves | Sinusoidal multiplier from 0.6 to 2.0 with a 300-second period. |
| Bursts | Multiplier 3.0 during the first 40 seconds of each 240-second interval, otherwise 0.7. |
| Resource mix | Constant arrival rate; swap numerical CPU/memory requests during seconds 600–1200, within node limits. |

Ten seeds (1001–1010), six patterns, and two loads give 120 traces. Each has 256 seconds of demand prehistory, 300 seconds of warm-up, 1,800 seconds of measured arrivals, and up to 120 seconds to complete remaining jobs. Every policy receives identical arrivals, requested resources, and actual durations for a given trace. We reuse the same traces as the preceding studies; they are not unseen evaluation data.

### 2.3 Evidence and intervention

All Jev arms receive current queue depth, oldest wait, running-job count, per-node free CPU and memory, node lifecycle and startup progress, capacity bounds, time since the last change, legal choices, and recent offered demand. Both receive 256 seconds of offered-demand history summarized as 16-second means.

The new `performance` input adds the following for **(now−60 s, now]** and **(now−120 s, now−60 s]**, clipped at simulation start:

| Signal | Meaning |
|---|---|
| Observation coverage | Seconds observed in the window; counts accompanying each latency distribution. |
| Offered, completed, rejected | Event-time counts and per-second rates. |
| Rejected fraction | Rejections divided by completions plus rejections in the window. This is not an arrival-cohort rejection probability. |
| Queue wait | Completed jobs' time between arrival and start: mean, p25, p50, p99 in seconds. |
| Completion latency | Completed jobs' time between arrival and observed completion: mean, p25, p50, p99 in seconds. |
| Capacity | Active node-seconds and average active nodes, including starting and draining nodes. |
| Resource efficiency | Reserved CPU and memory divided by total active capacity integrated over the window. |
| Recent actions | Applied capacity changes during the last 120 seconds, with timestamps. |

Empty samples and zero denominators are null. Latency summaries exclude unfinished and rejected jobs; current backlog and rejection signals remain visible. The collector reads completed events, not hidden durations or scheduled completion times. Resource integration uses the state that held during each one-second interval. The two windows let the model compare recent observations with the immediately preceding period, but do not establish that an action caused any change.

The efficiency-v1 objective remains verbatim. An appended paragraph defines the new fields and asks Jev to use the observed trends when balancing the same objectives. No numerical delay/cost weight, new utilization target, or longer cooldown is introduced. Consequently, the intervention is **feedback plus its interpretation instructions**, not a field-by-field ablation or online model training.

### 2.4 Toto and comparison policies

Toto is the Datadog time-series forecasting model used to provide demand forecasts. It receives three raw one-second series: offered jobs/s, CPU work/s, and memory work/s, where work uses requested resources times estimated duration. It uses 256 seconds of lookback to forecast 120 seconds ahead. Forecast-enabled Jev receives ten-second averages of pointwise p10/p50/p90 estimates with age and provenance. These are not quantiles of aggregate work. Forecasts were recorded every 20 seconds in the original study and are reused byte-for-byte here; this follow-up makes no new Toto calls.

We compare the new feedback arms primarily with their corresponding efficiency-prompt arms. We also retain the original deadline-first Jev arms and seven classical references: fixed two/six nodes, reactive demand targeting, an HPA-inspired utilization controller, EWMA demand smoothing, persistence forecasting, and Toto with reactive control. Persistence and reactive are equivalent under the existing maximum rule. Classical parameters were selected on separate development seeds for the original deadline objective and are not retuned here. The HPA-inspired controller is a simulation policy, not Kubernetes HPA itself.

All Jev arms use `jev-1.13.0`. Decisions occur every ten simulated seconds; decisions and forecasts become available after one simulated second in the primary experiment. Actual API latency is recorded but does not drive simulated time. The client deadline is five seconds, without automatic retries. An ordinary inference failure invokes the recorded reactive fallback. A single legal action needs no model call. Billing/authentication failures stop collection. Each new arm has one model realization per trace; model stochasticity and service drift across collection periods are not separately estimated.

## 3. Measurements and verification

We report queue wait and arrival-to-completion mean, p25, p50, and p99 in seconds for completed jobs arriving during measurement. Pooled summaries are request-weighted. Rejected and unfinished jobs are reported separately so that dropping difficult jobs cannot silently improve the delay statistics. The 60-second queue expiry can constrain tail latency.

Capacity is mean active node-hours per trace, including the completion period. Utilization is resource reservation divided by active capacity. Scaling behavior includes applied drains and direction reversals within 120 seconds. Inference accounting covers warm-up and measurement calls, including failures with reported usage.

Paired effects are equally weighted per-trace differences. Confidence intervals use 10,000 bootstrap resamples of the ten seed blocks, keeping all workload/load cells of a seed together. These intervals characterize variation across this seed set; they do not account for independent repeated model draws. Pooled-table differences need not equal paired trace-mean differences.

All 120 traces replayed with exact evidence and summary equality, without inference calls. Independent checks reconstructed 100,800 feedback windows from final job journals and lifecycle events, including every latency distribution, event-time count, resource integral, and action timestamp. Workload and forecast files match the original study byte-for-byte; terminal outcome counts and per-call charges reconcile.

## 4. Results

### 4.1 Delay, rejection, and capacity

Jev · feedback used 2.333 active node-hours per trace (-2.9% versus Jev · efficiency), with pooled mean queue wait 23.15 versus 21.58 seconds and rejection 3.26% versus 2.93%. Pooled mean arrival-to-completion time changed from 32.05 to 33.60 seconds. Applied drains changed from 3,078 to 2,845; rapid direction reversals changed from 1,828 to 1,491.

Jev + Toto · feedback used 2.251 active node-hours per trace (-2.1% versus Jev + Toto · efficiency), with pooled mean queue wait 24.66 versus 23.49 seconds and rejection 3.37% versus 3.27%. Pooled mean arrival-to-completion time changed from 33.94 to 35.09 seconds. Applied drains changed from 2,876 to 2,656; rapid direction reversals changed from 1,722 to 1,445.

**Table 1. Completed-job queue wait (seconds).**

| Policy | Mean | p25 | p50 | p99 |
| --- | --- | --- | --- | --- |
| Fixed 2 | 51.04 | 50.00 | 56.00 | 59.00 |
| Fixed 6 | 7.22 | 0.00 | 0.00 | 58.00 |
| Reactive | 8.37 | 0.00 | 0.00 | 58.00 |
| HPA-inspired | 7.69 | 0.00 | 0.00 | 58.00 |
| EWMA | 9.06 | 0.00 | 0.00 | 59.00 |
| Persistence | 8.37 | 0.00 | 0.00 | 58.00 |
| Toto + reactive | 7.22 | 0.00 | 0.00 | 58.00 |
| Jev · original | 7.34 | 0.00 | 0.00 | 58.00 |
| Jev + Toto · original | 7.43 | 0.00 | 0.00 | 58.00 |
| Jev · efficiency | 21.58 | 1.00 | 16.00 | 59.00 |
| Jev + Toto · efficiency | 23.49 | 4.00 | 19.00 | 59.00 |
| Jev · feedback | 23.15 | 3.00 | 18.00 | 59.00 |
| Jev + Toto · feedback | 24.66 | 5.00 | 21.00 | 59.00 |

**Table 2. Arrival-to-completion time (seconds).**

| Policy | Mean | p25 | p50 | p99 |
| --- | --- | --- | --- | --- |
| Fixed 2 | 60.41 | 59.00 | 64.00 | 80.00 |
| Fixed 6 | 17.76 | 7.00 | 13.00 | 72.00 |
| Reactive | 18.91 | 7.00 | 13.00 | 72.00 |
| HPA-inspired | 18.23 | 7.00 | 13.00 | 72.00 |
| EWMA | 19.60 | 7.00 | 14.00 | 72.00 |
| Persistence | 18.91 | 7.00 | 13.00 | 72.00 |
| Toto + reactive | 17.76 | 7.00 | 13.00 | 72.00 |
| Jev · original | 17.88 | 7.00 | 13.00 | 72.00 |
| Jev + Toto · original | 17.97 | 7.00 | 13.00 | 72.00 |
| Jev · efficiency | 32.05 | 14.00 | 27.00 | 76.00 |
| Jev + Toto · efficiency | 33.94 | 15.00 | 30.00 | 76.00 |
| Jev · feedback | 33.60 | 15.00 | 29.00 | 76.00 |
| Jev + Toto · feedback | 35.09 | 16.00 | 32.00 | 77.00 |

**Table 3. Resource consumption, loss, and control activity.** Node-hours are averaged per trace; drains and reversals are totals across 120 traces.

| Policy | Node-hours/trace | CPU reserved | Memory reserved | Rejected | Unfinished | Drains | Reversals |
| --- | --- | --- | --- | --- | --- | --- | --- |
| Fixed 2 | 1.033 | 72.4% | 65.7% | 25.05% | 0 | 0 | 0 |
| Fixed 6 | 3.022 | 40.1% | 38.6% | 0.96% | 0 | 0 | 0 |
| Reactive | 2.873 | 42.2% | 40.6% | 0.99% | 0 | 1066 | 938 |
| HPA-inspired | 2.922 | 41.5% | 40.0% | 0.98% | 0 | 339 | 228 |
| EWMA | 2.760 | 43.9% | 42.2% | 1.03% | 0 | 1018 | 769 |
| Persistence | 2.873 | 42.2% | 40.6% | 0.99% | 0 | 1066 | 938 |
| Toto + reactive | 3.022 | 40.1% | 38.6% | 0.96% | 0 | 2 | 2 |
| Jev · original | 3.001 | 40.4% | 38.9% | 0.96% | 0 | 1 | 0 |
| Jev + Toto · original | 2.971 | 40.8% | 39.3% | 0.96% | 0 | 0 | 0 |
| Jev · efficiency | 2.402 | 49.2% | 47.0% | 2.93% | 0 | 3078 | 1828 |
| Jev + Toto · efficiency | 2.300 | 51.1% | 48.8% | 3.27% | 0 | 2876 | 1722 |
| Jev · feedback | 2.333 | 50.4% | 48.0% | 3.26% | 0 | 2845 | 1491 |
| Jev + Toto · feedback | 2.251 | 52.2% | 49.7% | 3.37% | 0 | 2656 | 1445 |

<figure><img src="delay-capacity.png" alt="Queue delay against active node-hours for all policies"><figcaption>Figure 1. Delay versus capacity. Lower on both axes is preferable; rejection must also be considered. The right panel excludes Fixed 2 to expand the scale.</figcaption></figure>

<figure><img src="delay-distributions.png" alt="Completed-job queue and completion latency distributions"><figcaption>Figure 2. Direct latency distributions for the principal comparisons. Use the controls to select multiple policies.</figcaption></figure>

### 4.2 Paired comparisons

**Table 4. Mean paired change [95% seed-block bootstrap interval].** Each row subtracts the indicated baseline from the new policy; negative values mean less delay, capacity, or rejection. Rejection is in percentage points.

| Policy − baseline | Mean queue wait (s) | Node-hours | Rejection (pp) |
| --- | --- | --- | --- |
| Jev · feedback − Jev · efficiency | +1.67 [+0.96, +2.45] | -0.069 [-0.077, -0.060] | +0.35 [+0.20, +0.49] |
| Jev + Toto · feedback − Jev + Toto · efficiency | +1.14 [+0.61, +1.55] | -0.049 [-0.057, -0.040] | +0.08 [-0.13, +0.29] |
| Jev + Toto · feedback − Jev · feedback | +1.50 [+1.07, +1.99] | -0.082 [-0.091, -0.074] | +0.07 [-0.01, +0.15] |
| Jev · feedback − HPA-inspired | +15.77 [+15.16, +16.43] | -0.589 [-0.610, -0.568] | +2.26 [+2.01, +2.54] |
| Jev + Toto · feedback − HPA-inspired | +17.27 [+16.70, +17.78] | -0.671 [-0.696, -0.646] | +2.33 [+2.07, +2.60] |

For Jev · feedback, the paired queue-wait change is +1.67 [+0.96, +2.45] seconds; capacity changes by -0.069 [-0.077, -0.060] node-hours and rejection by +0.35 [+0.20, +0.49] percentage points. These paired intervals use equal trace weights, unlike the pooled latency tables.

This arm trades longer mean delay for less capacity; the acceptable operating point depends on an explicit service-quality budget.

For Jev + Toto · feedback, the paired queue-wait change is +1.14 [+0.61, +1.55] seconds; capacity changes by -0.049 [-0.057, -0.040] node-hours and rejection by +0.08 [-0.13, +0.29] percentage points. These paired intervals use equal trace weights, unlike the pooled latency tables.

This arm trades longer mean delay for less capacity; the acceptable operating point depends on an explicit service-quality budget.

Within the new feedback experiment, adding Toto changes mean per-trace queue wait by +1.50 [+1.07, +1.99] seconds, node-hours by -0.082 [-0.091, -0.074], and rejection by +0.07 [-0.01, +0.15] percentage points. This comparison uses identical forecast records across the earlier and new forecast-enabled arms.

### 4.3 Capacity over time

<figure><img src="capacity-moderate.png" alt="Capacity decisions and traffic for moderate load"><figcaption>Figure 3. Mean active capacity across ten seeds, moderate load. Dotted traffic uses the right axis. The selector can show CPU or memory work instead.</figcaption></figure>

<figure><img src="capacity-heavy.png" alt="Capacity decisions and traffic for heavy load"><figcaption>Figure 4. The same comparison under heavy load. Workload overlays are observed offered demand in 30-second bins and do not represent foresight available to the policy.</figcaption></figure>

### 4.4 Inference cost and failures

The new arms made 26,108 Jev calls, with $4.489563 in known estimated charges. There were 34 evaluation failures using the existing fallback, 34 calls without usage, and 0 calls with unpriced model identifiers. No billing/authentication failure occurred. There were no additional paid preflight runs. Including earlier experiments, cumulative known estimated Jev charges are $12.530728.

| Arm | Calls | Input tokens | Known estimated USD | Missing usage |
| --- | --- | --- | --- | --- |
| Jev · original | 19,896 | 34,341,457 | $1.442341 | 5 |
| Jev + Toto · original | 19,885 | 77,638,938 | $3.260835 | 5 |
| Jev · efficiency | 10,544 | 20,433,579 | $0.858210 | 17 |
| Jev + Toto · efficiency | 12,271 | 50,323,141 | $2.113572 | 45 |
| Jev · feedback | 11,936 | 34,797,818 | $1.461508 | 13 |
| Jev + Toto · feedback | 14,172 | 72,096,549 | $3.028055 | 21 |

Costs are usage-based estimates at the frozen comparison rate of $0.042 per million input tokens and zero output-token charge, not a billing reconciliation or a fresh pricing quote. Missing usage is excluded from the estimate. No new Toto requests were made; Toto inference and infrastructure costs remain unpriced. Dollar costs are not inputs to Jev in this experiment; recent node-seconds provide the capacity-efficiency signal.

## 5. Limits and implications

The workload family, seeds, and qualitative objective were chosen after earlier results. This is an exploratory follow-up. Additional evidence and its explanatory instructions change together, and earlier arms were not freshly repeated. We therefore cannot attribute any difference solely to the feedback fields or claim a generally superior controller.

The simulator omits contention, network delay, failures, placement changes, and heterogeneous nodes. Equalized decision latency also omits the operational cost of slower inference. Feedback windows are event-time summaries: completions can describe jobs admitted before a window, and a controller can see good completed-job latency while difficult jobs remain queued. We mitigate that interpretive risk with explicit sample counts, rejection, and current backlog; we do not eliminate it.

A qualitative efficiency objective still leaves the acceptable tradeoff unspecified. A production comparison should fix a latency/loss budget or resource budget in advance, evaluate policies at matched budgets, repeat independent model draws, and test fresh workloads. More history does not replace deterministic execution guards, nor does it by itself train or calibrate the model.

Recent performance feedback is now an explicit, auditable part of the controller input. The observed results above describe its operating point under this objective; they do not establish an optimal latency–capacity balance or replace the need to specify acceptable loss and delay.

## Reproducibility

The frozen [protocol](FEEDBACK_PROTOCOL.md), [prompt](feedback-v1.txt), [per-trace data](per-trace.csv), [summary](summary.json), and [validation record](validation.json) accompany this report. Raw workload, forecast, decision, timeline, and job recordings remain in the repository output directory. Exact replay requires no model access. The earlier efficiency study is available [here](EFFICIENCY_PAPER.md).

## Appendix: workload-level queueing delay

Mean completed-job queue wait in seconds, averaged over ten trace means. Bold indicates ties at the displayed precision for the lowest value in each row; it does not imply statistically significant superiority.

| Workload | Load | Fixed 2 | Fixed 6 | Reactive | HPA-inspired | EWMA | Persistence | Toto + reactive | Jev · original | Jev + Toto · original | Jev · efficiency | Jev + Toto · efficiency | Jev · feedback | Jev + Toto · feedback |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| steady | moderate | 49.45 | **0.01** | 0.04 | 0.04 | 0.19 | 0.04 | **0.01** | 0.02 | 0.02 | 9.42 | 13.09 | 12.01 | 13.24 |
| steady | heavy | 56.02 | **0.09** | 0.26 | 0.10 | 0.31 | 0.26 | **0.09** | **0.09** | **0.09** | 9.72 | 11.53 | 10.12 | 14.05 |
| spike | moderate | 34.55 | **3.86** | 6.44 | 6.30 | 6.90 | 6.44 | **3.86** | 5.23 | 5.80 | 19.76 | 23.51 | 24.91 | 25.00 |
| spike | heavy | 55.00 | **16.18** | 16.57 | 16.42 | 16.83 | 16.57 | **16.18** | **16.18** | 16.32 | 24.13 | 26.57 | 25.83 | 25.66 |
| ramp | moderate | 45.85 | **0.64** | 1.47 | 0.68 | 1.66 | 1.47 | **0.64** | 0.67 | 0.95 | 10.23 | 13.17 | 12.15 | 14.72 |
| ramp | heavy | 56.58 | 27.99 | 29.04 | 27.99 | 29.46 | 29.04 | 27.99 | 28.00 | **27.97** | 34.38 | 35.72 | 35.61 | 36.94 |
| waves | moderate | 50.59 | **0.28** | 0.99 | 0.72 | 1.96 | 0.99 | **0.28** | **0.28** | **0.28** | 26.35 | 28.30 | 27.23 | 28.29 |
| waves | heavy | 56.30 | 7.55 | 9.20 | 7.62 | 10.83 | 9.20 | 7.55 | **7.53** | **7.53** | 30.20 | 31.13 | 32.15 | 33.24 |
| bursts | moderate | 40.57 | **1.72** | 4.03 | 3.79 | 4.45 | 4.03 | **1.72** | 2.02 | 2.14 | 27.51 | 27.44 | 27.89 | 29.79 |
| bursts | heavy | 53.89 | **8.74** | 10.75 | 9.85 | 13.04 | 10.75 | **8.74** | 8.87 | 8.91 | 29.23 | 29.30 | 29.24 | 29.61 |
| mix | moderate | 50.19 | **0.19** | 0.45 | 0.29 | 0.53 | 0.45 | **0.19** | **0.19** | 0.28 | 11.39 | 14.36 | 13.72 | 15.35 |
| mix | heavy | 56.04 | **3.34** | 4.94 | 3.35 | 5.51 | 4.94 | **3.34** | **3.34** | **3.34** | 14.06 | 16.65 | 15.58 | 18.52 |
