# Can Recent Performance Feedback Improve Model-Guided Autoscaling?

<p class="subtitle">A paired follow-up with observed latency, rejection, utilization, and scaling history</p>
<p class="byline">Technical report · September 2026 · Exploratory simulation study</p>

## Abstract

An autoscaling controller chooses how much capacity to keep available as demand changes. A model can see current queue pressure without knowing whether recent operation has delivered acceptable service. We test whether adding explicit performance feedback changes Jev's balance between delay and resource consumption. Two new arms—Jev and Jev with Toto forecasts—receive adjacent 60-second performance windows and recent scaling actions. The efficiency objective, simulator, workloads, model identifier, action guards, and recorded forecasts remain fixed. We run each arm on 120 paired traces and compare 240 new outcomes with 1,320 prior outcomes. {{abstract_result}} The intervention combines additional evidence with instructions explaining that evidence; this is an exploratory comparison on previously examined workloads, not a held-out causal demonstration.

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

{{validation_result}}

## 4. Results

### 4.1 Delay, rejection, and capacity

{{result_description}}

**Table 1. Completed-job queue wait (seconds).**

{{queue_table}}

**Table 2. Arrival-to-completion time (seconds).**

{{completion_table}}

**Table 3. Resource consumption, loss, and control activity.** Node-hours are averaged per trace; drains and reversals are totals across 120 traces.

{{resource_table}}

<figure><img src="delay-capacity.png" alt="Queue delay against active node-hours for all policies"><figcaption>Figure 1. Delay versus capacity. Lower on both axes is preferable; rejection must also be considered. The right panel excludes Fixed 2 to expand the scale.</figcaption></figure>

<figure><img src="delay-distributions.png" alt="Completed-job queue and completion latency distributions"><figcaption>Figure 2. Direct latency distributions for the principal comparisons. Use the controls to select multiple policies.</figcaption></figure>

### 4.2 Paired comparisons

**Table 4. Mean paired change [95% seed-block bootstrap interval].** Each row subtracts the indicated baseline from the new policy; negative values mean less delay, capacity, or rejection. Rejection is in percentage points.

{{paired_table}}

{{interpretation}}

### 4.3 Capacity over time

<figure><img src="capacity-moderate.png" alt="Capacity decisions and traffic for moderate load"><figcaption>Figure 3. Mean active capacity across ten seeds, moderate load. Dotted traffic uses the right axis. The selector can show CPU or memory work instead.</figcaption></figure>

<figure><img src="capacity-heavy.png" alt="Capacity decisions and traffic for heavy load"><figcaption>Figure 4. The same comparison under heavy load. Workload overlays are observed offered demand in 30-second bins and do not represent foresight available to the policy.</figcaption></figure>

### 4.4 Inference cost and failures

{{cost_result}}

{{cost_table}}

Costs are usage-based estimates at the frozen comparison rate of $0.042 per million input tokens and zero output-token charge, not a billing reconciliation or a fresh pricing quote. Missing usage is excluded from the estimate. No new Toto requests were made; Toto inference and infrastructure costs remain unpriced. Dollar costs are not inputs to Jev in this experiment; recent node-seconds provide the capacity-efficiency signal.

## 5. Limits and implications

The workload family, seeds, and qualitative objective were chosen after earlier results. This is an exploratory follow-up. Additional evidence and its explanatory instructions change together, and earlier arms were not freshly repeated. We therefore cannot attribute any difference solely to the feedback fields or claim a generally superior controller.

The simulator omits contention, network delay, failures, placement changes, and heterogeneous nodes. Equalized decision latency also omits the operational cost of slower inference. Feedback windows are event-time summaries: completions can describe jobs admitted before a window, and a controller can see good completed-job latency while difficult jobs remain queued. We mitigate that interpretive risk with explicit sample counts, rejection, and current backlog; we do not eliminate it.

A qualitative efficiency objective still leaves the acceptable tradeoff unspecified. A production comparison should fix a latency/loss budget or resource budget in advance, evaluate policies at matched budgets, repeat independent model draws, and test fresh workloads. More history does not replace deterministic execution guards, nor does it by itself train or calibrate the model.

{{conclusion}}

## Reproducibility

The frozen [protocol](FEEDBACK_PROTOCOL.md), [prompt](feedback-v1.txt), [per-trace data](per-trace.csv), [summary](summary.json), and [validation record](validation.json) accompany this report. Raw workload, forecast, decision, timeline, and job recordings remain in the repository output directory. Exact replay requires no model access. The earlier efficiency study is available [here](EFFICIENCY_PAPER.md).

## Appendix: workload-level queueing delay

Mean completed-job queue wait in seconds, averaged over ten trace means. Bold indicates ties at the displayed precision for the lowest value in each row; it does not imply statistically significant superiority.

{{workload_table}}
