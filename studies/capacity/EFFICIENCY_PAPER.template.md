# Balancing Delay and Capacity in Model-Guided Autoscaling

<p class="subtitle">A paired follow-up experiment with an efficiency-aware Jev instruction</p>
<p class="byline">Technical report · September 2026 · Exploratory prompt follow-up</p>

## Abstract

A model-guided autoscaler can achieve low delay by retaining more capacity than the workload needs. This report tests whether explicitly asking a decision model to reclaim idle capacity changes that behavior. We compare the original and an efficiency-aware Jev instruction in a synthetic cluster with heterogeneous CPU- and memory-reserving jobs. Both variants execute through the same deterministic Reflex state machine; each is evaluated with and without Toto forecast evidence. The follow-up adds 240 policy runs on the original 120 paired workload traces, reusing previously collected forecasts and classical-policy outcomes. The new prompt is frozen before this follow-up's evaluation. We report queueing delay and arrival-to-completion time directly in seconds, including mean, p25, p50, and p99, alongside capacity consumption, rejection, and scaling behavior. {{abstract_result}} These are observed tradeoffs for a qualitative instruction change, not proof that the controller optimizes a defined latency-cost utility.

## 1. Motivation and research question

An autoscaler changes the number of worker nodes available to execute incoming jobs. More nodes can shorten queues, but nodes that remain idle still consume resources. The appropriate decision therefore depends on how a system values responsiveness relative to capacity consumption.

In the preceding experiment, Jev generally scaled up and retained near-maximum capacity. The original instruction did mention efficiency, but made it subordinate to a completion deadline: complete jobs within their estimated duration plus 20 seconds, then minimize capacity subject to that objective. This ordering may make retaining extra capacity a reasonable response to the instruction, even if a less conservative operating point would be preferable to a system owner.

We test a different instruction that treats short queues and resource efficiency as competing goals. It explicitly asks Jev to drain excess nodes when demand falls, consider both CPU and memory, retain moderate headroom, and avoid treating every forecast upper bound as a provisioning requirement. The question is whether this instruction changes capacity use and, if so, what happens to actual delays, rejection, and control stability.

This is **autoscaling**, not a test of job-placement algorithms. Incoming workloads vary over time, but every policy receives the same arrivals and actual execution durations for a given trace. Placement remains strict FIFO first-fit. We change the Jev instruction, not the simulator, observation schema, action set, model version, or executor rules.

## 2. System and experimental design

### 2.1 Models and deterministic execution

Jev is TypeSafe AI's structured decision model. Here it receives a serialized system state and selects a legal capacity action, rather than generating an unconstrained operating command ([TypeSafe AI](https://typesafe.ai/blog/introducing-system-one-models-and-jev)). Toto is Datadog's time-series forecasting model; here it predicts offered demand and supplies optional evidence to the controller ([Toto research paper](https://arxiv.org/abs/2505.14766)). The forecasting deployment's configured identity is recorded but is not verified by its responses.

Reflex is the Rust state-machine library used to execute proposals. A node is off, starting, ready, or draining. The action choices are hold, start one, start two, and drain one. Starting a node takes 60 seconds; draining lets existing jobs finish but prevents new assignments. Starting and draining nodes consume capacity and count against the six-node limit. At least one ready node must remain. Actions are separated by a ten-second cooldown; draining requires an empty queue and no other drain in progress. The executor checks observation freshness and lifecycle revision at execution time, and invariants check resource reservations and accounting.

A transition that passes these checks is permitted by the implemented rules. It is not guaranteed to be efficient, and the checks do not constitute a formal proof of the whole system. Every tested policy uses this same boundary.

### 2.2 Simulator and workload

The simulator has six homogeneous nodes, each with eight CPU units and 16 GiB of memory; two are initially ready. Jobs reserve their requested resources until completion. There is no preemption, migration, CPU sharing, or load-dependent execution slowdown. Time advances in one-second ticks.

Jobs enter a FIFO queue with a maximum of 128 entries and a 60-second expiry. The oldest queued job is placed on the first ready node with sufficient CPU and memory. If it fits nowhere, smaller jobs behind it cannot bypass it. Thus resource fragmentation and head-of-line blocking can create delay even when aggregate capacity appears underused.

| Client class | Base arrivals/s | CPU/job | Memory/job | Estimated duration |
|---|---:|---:|---:|---:|
| Small | 0.35 | 1 | 1 GiB | 6 s |
| CPU-heavy | 0.16 | 4 | 2 GiB | 12 s |
| Memory-heavy | 0.14 | 1 | 8 GiB | 18 s |

Each class has seeded Poisson arrivals. Actual duration is the ceiling of estimated duration multiplied by a uniform value from 0.75 to 1.25; policies never observe that hidden duration in advance. Each trace has 256 seconds of prehistory, five minutes of warm-up, 30 minutes of measured arrivals, and at most 120 seconds to complete remaining work. Resource consumption is counted during measurement and completion, including starting and draining capacity. Inference costs also include warm-up.

The six workload patterns are steady traffic; a five-minute spike; a gradual ramp, plateau, and decline; five-minute sinusoidal waves; 40-second bursts every four minutes; and a temporary swap of the numerical CPU/memory requests while arrival rates remain constant. Heavy load multiplies the moderate arrival rates by 1.5. The following figure gives their exact configured shapes.

<figure id="workload-patterns"><a href="workload-patterns.png"><img src="workload-patterns.png" alt="The six configured arrival and resource-demand patterns"></a><figcaption>Figure 1. Expected workload shapes. Individual arrivals fluctuate randomly. Resource-mix curves normalize CPU and memory work to their own usual levels; they are not comparable absolute resource quantities.</figcaption></figure>

There are ten seeds for each of the six patterns and two load levels, giving 120 paired traces. Pattern timing is fixed; seeds change arrivals and actual durations. The same seeds were already examined in the original study, so this follow-up is not an independent unseen test of a hypothesis formed after those results.

### 2.3 What changes in the instruction

The original instruction prioritized completing jobs before a fixed SLO deadline, with node-seconds secondary. The new instruction explicitly balances queue delay and efficient capacity. It asks the model to infer resource use from free CPU and memory, distinguish sustained demand from short spikes, and favor a legal drain when fewer nodes can safely handle the observed demand. It also cautions about startup delay, per-node packing, memory constraints, and rapid reversals.

The new instruction supplies neither a numerical latency-cost weight nor a fixed utilization target. Consequently, this experiment tests an efficiency-aware heuristic preference, not an optimizer with a unique mathematically specified solution. Several related clauses change together; the result does not isolate the causal effect of a single sentence. The complete prompt appears in Appendix B and was frozen before the follow-up's evaluation.

Both Jev arms use `jev-1.13.0`. Evidence includes queue size, oldest wait, running jobs, per-node free resources and lifecycle, startup progress, recent demand, time since the last change, and legal choices. Both also receive 256 seconds of offered-demand history compressed into 16-second means. Forecast-enabled arms receive ten-second summaries of Toto's pointwise p10/p50/p90 predictions. No scenario name, seed, future arrival, or actual execution duration is supplied.

Toto originally received raw one-second history and produced a 120-second forecast every 20 seconds. This follow-up reuses the exact recorded forecasts; it makes no new Toto requests. Negative forecast demand is clamped to zero for decisions. The available context is shorter than the five-minute wave period, and pointwise quantile averages are not forecasts of aggregate mean demand.

### 2.4 Comparison policies and timing

We retain the original nine policies and add two efficiency-prompt variants:

| Policy | Definition |
|---|---|
| Fixed 2 / Fixed 6 | Retain two or six nodes, respectively; fixed-six expands during warm-up. |
| Reactive | Convert ten-second observed CPU/memory work into a desired count using a 0.6 target fraction; account for starting nodes, rescue queues, and wait 120 seconds before eligible downscales. |
| HPA-inspired | Use reserved resources relative to a target, a 10% tolerance, downscale stabilization, and queue rescue. This is a modified simulated policy, not Kubernetes HPA. |
| EWMA | Apply the reactive target calculation to exponentially smoothed demand, with coefficient 0.6. |
| Persistence | Constant demand forecast; mathematically identical to reactive control under this implementation's maximum rule. |
| Toto + reactive | Size using the maximum of observed demand and future mean-p90 buckets, with reactive parameters unchanged. |
| Jev / Jev + Toto · original | Model-selected actions using the original deadline-first instruction, without/with forecasts. |
| Jev / Jev + Toto · efficiency | The two new arms, using the frozen efficiency-aware instruction, without/with the same forecasts. |

Classical parameters were selected on separate development seeds 11–13 using the original SLO-based criterion and a small grid of targets, downscale waits, and smoothing coefficients. They are not retuned for the new latency-capacity preference. Kubernetes HPA is a conceptual reference for the utilization-ratio baseline, not an implementation-equivalent competitor ([Kubernetes documentation](https://kubernetes.io/docs/concepts/workloads/autoscaling/horizontal-pod-autoscale/)).

All decisions occur every ten simulated seconds. The primary experiment gives each decision and forecast a one-second availability delay; arrivals and lifecycle events advance before execution. API latency is recorded but does not determine this equalized delay. Jev has a five-second deadline, and the runner makes no automatic inference retries. Ordinary evaluation failures use the recorded reactive fallback. Billing and authorization failures stop the experiment rather than being accepted as model outcomes. When only one action is legal, the action is selected without calling Jev.

## 3. Measurement and analysis

### 3.1 Report delay directly

For a completed job, **queueing delay** is the time from arrival until execution starts. **Arrival-to-completion time** is queueing delay plus actual execution duration. We report mean, p25, p50, and p99 of each in seconds. A zero median queue wait therefore does not imply instantaneous job completion.

The main distribution tables pool all completed measured-period jobs within an arm. They are request-weighted: workload cells with more completed jobs contribute more observations. Quantiles use the higher order statistic at index `ceil((n − 1) × p)`. A p99 is an actual quantile of the pooled observations, not an average of per-run percentiles.

Rejected and unfinished jobs have no completed-job latency and are excluded from these distributions. Their rates and counts appear beside latency. A policy cannot be judged more efficient merely because it drops work that would otherwise be slow. The 60-second queue expiry also truncates completed-job waits, which can make p99 values similar even when queueing and rejection differ substantially.

### 3.2 Resources, scaling, and uncertainty

We report active node-hours, CPU and memory reservation utilization, applied start/drain operations, and direction reversals within 120 seconds. Resource utilization measures reserved simulator capacity, not operating-system CPU activity. Scaling counts cover the measured period and completion period; inference costs include warm-up.

For paired comparisons, we separately compute the mean delay within each trace and weight traces equally. The tradeoff figure uses these macro means so high-volume workload cells do not dominate. Paired 95% intervals resample ten complete seed blocks 10,000 times, retaining all workload cells and policies within each seed. Differences in per-trace p99 are also available in the data. These are exploratory intervals, not multiplicity-adjusted confirmatory tests, and they do not isolate independent model-response randomness.

Known Jev cost uses reported input tokens priced at the study's frozen $0.042 per million tokens; output tokens are free at that recorded price ([TypeSafe model documentation](https://docs.typesafe.ai/models)). Missing usage and unknown Toto monetary cost remain explicitly excluded. Simulated node-hours and inference dollars are reported separately.

## 4. Results

### 4.1 Completed-job delays

**Table 1. Queueing delay in seconds, pooled over completed jobs. Rejection and unfinished counts refer to all offered measured-period jobs. Lower delay must be interpreted together with these outcomes.**

{{queue_table}}

**Table 2. Arrival-to-completion time in seconds, using the same completed-job cohort. These values include execution time; Table 1 does not.**

{{completion_table}}

<figure><img src="delay-distributions.png" alt="Mean and p25, p50, p99 of completed-job queueing and completion times"><figcaption>Figure 2. Direct delay distributions for selected comparison policies. The plotted statistics pool completed jobs, and do not assign a finite completion time to rejected jobs.</figcaption></figure>

### 4.2 Capacity use and scale-down behavior

**Table 3. Resource and control behavior. Node-hours are averaged equally across traces. Utilization is total reservation-seconds divided by total active resource-seconds. Start/drain counts are applied operations after warm-up; a start operation may add one or two nodes.**

{{resource_table}}

{{scaling_result}}

<figure><img src="capacity-moderate.png" alt="Active-node timelines comparing original and efficiency-aware Jev across moderate workloads"><figcaption>Figure 3. Active capacity over time at moderate load, averaged over ten seeds. Original Jev arms use dashed lines; efficiency-aware arms use solid lines. Starting and draining nodes remain active for accounting purposes. The interactive workload overlay uses the right axis; its 30-second bins average actual offered workload across the same ten seeds. The static figure shows request rate.</figcaption></figure>

<figure><img src="capacity-heavy.png" alt="Active-node timelines comparing original and efficiency-aware Jev across heavy workloads"><figcaption>Figure 4. The same capacity comparison under heavy load. Use the workload selector to overlay request rate, CPU work, or memory work on the right axis. A mean curve can conceal variation between runs; per-trace timelines are retained.</figcaption></figure>

### 4.3 Paired tradeoffs

**Table 4. Changes relative to the paired reference. Delay changes are differences between equally weighted per-trace means, not differences between pooled percentiles. Brackets give 95% seed-cluster bootstrap intervals. Negative node-hours save capacity; negative queue delay shortens waits; positive rejection changes lose more offered work.**

{{paired_table}}

<figure><img src="delay-capacity.png" alt="Mean per-trace queue delay plotted against mean active node-hours"><figcaption>Figure 5. Capacity–delay tradeoffs. Lower values on both axes are preferable, but rejection remains a separate outcome. The expanded view excludes the overloaded fixed-two reference.</figcaption></figure>

{{tradeoff_result}}

### 4.4 Inference expense

**Table 5. Inference calls and known estimated cost for the four Jev arms. Original-arm costs are historical; they were not incurred again for this follow-up.**

{{inference_table}}

{{cost_result}}

## 5. Interpretation and limitations

The strongest conclusion this experiment can establish is that a particular instruction changes the controller's operating point. A lower node-hour total shows less capacity consumption. It does not establish greater efficiency at a fixed service-quality requirement if delays or rejection also increase. Likewise, more drain actions alone are not evidence of better behavior: repeated drains followed by restarts can indicate oscillation.

The earlier instruction was not indifferent to cost; it subordinated cost to a completion deadline. The new instruction expresses a different preference. Any observed difference combines that preference change, related heuristic guidance, new model responses, and collection at a later time. It is not a clean causal estimate of one phrase or of model capability in general.

The completion-time distributions are conditional on successful completion and influenced by queue expiry. Fixed-two remains an overloaded reference. Intermediate fixed capacities and comparisons at matched resource budgets would help identify whether adaptive policies improve the attainable capacity–delay tradeoff. Those extensions are not part of this follow-up.

The simulator uses strict FIFO, hard CPU/memory reservations, and no contention slowdown or migration. Classical baselines retain their original SLO-oriented tuning. There are only ten independent seed blocks, fixed workload timings, and one live model execution per arm per trace. Toto's pointwise forecast representation and short context may limit its control usefulness; its model version and monetary cost are not independently verified. The deterministic Reflex boundary is shared across all arms, not ablated as an experimental treatment. These limits prevent a production-general or optimality claim.

## 6. Conclusion

{{conclusion}}

## Reproducibility

The new experiment comprises 120 paired traces and 240 new policy runs, compared with the 1,080 preserved original policy outcomes. The original and follow-up workload files and forecast files are checked for byte identity. Validation reconciles offered-job identities, completion/rejection accounting, recorded usage and cost, the frozen prompt, absence of billing failures, and exact no-network replay of every new trace.

The local report package includes [per-trace data](per-trace.csv), [summary statistics](summary.json), [validation](validation.json), and the [follow-up protocol](EFFICIENCY_PROTOCOL.md). The original [paper](initial-paper.html) remains available as the record of the first experiment. This is a technical report, not a peer-reviewed publication.

## Appendix A. Delay by workload

**Table A1. Mean completed-job queueing delay in seconds, pooled within each workload/load cell. Bold identifies the lowest observed mean and includes ties; it does not establish statistical significance. Rejection rates appear in Table A2 so selection of completed jobs remains visible.**

{{workload_delay_table}}

**Table A2. Rejection rates within the same cells. All offers, not only completions, are the denominator.**

{{workload_rejection_table}}

## Appendix B. Frozen efficiency-aware instruction

> {{prompt}}

## References

1. TypeSafe AI. [Introducing System One Models & Jev](https://typesafe.ai/blog/introducing-system-one-models-and-jev). September 2026. Background on the structured decision interface.
2. TypeSafe AI. [Models](https://docs.typesafe.ai/models). Source of the model identifier and the price recorded for the study.
3. [This Time is Different: An Observability Perspective on Time Series Foundation Models](https://arxiv.org/abs/2505.14766). 2025. Background on Toto; not verification of the deployment used here.
4. Kubernetes contributors. [Horizontal Pod Autoscaling](https://kubernetes.io/docs/concepts/workloads/autoscaling/horizontal-pod-autoscale/). Background on utilization-ratio control; the tested baseline is simplified and modified.
