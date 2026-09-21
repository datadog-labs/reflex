# Model-Guided Autoscaling Under Deterministic Constraints

<p class="subtitle">A paired simulation study of Jev decisions and Toto forecasts</p>
<p class="byline">Technical report · September 2026 · Initial empirical study</p>

## Abstract

Autoscaling controllers must add capacity before queues become costly while avoiding unnecessary resource consumption. This study evaluates whether a model that selects discrete actions can improve this tradeoff, and whether time-series forecasts provide additional value. We compare Jev, a structured decision model, with fixed-capacity and classical feedback policies in a synthetic cluster that serves heterogeneous CPU- and memory-reserving jobs. All policies execute through Reflex, a Rust library that checks proposed state transitions against deterministic constraints. The experiment comprises six workload patterns, two load levels, and ten held-out seeds: 120 paired workload traces and 1,080 policy runs. Jev improves mean on-time completion by 2.00 percentage points over a tuned reactive policy, at 4.44% greater capacity consumption. Relative to an HPA-inspired policy, the corresponding changes are +0.83 points and +2.69%. Adding Toto forecasts to Jev reduces capacity consumption by 1.00% but reduces on-time completion by 0.19 points (exploratory 95% paired bootstrap interval: −0.40 to −0.01). Fixed-six capacity attains slightly higher on-time completion than either Jev arm. Known Jev inference cost is $4.70 for the primary experiment; Toto monetary cost is unavailable. These results show a service-quality/resource tradeoff, rather than a demonstrated efficiency advantage for model-guided control or an improvement in completion quality from this forecast pipeline.

## 1. Introduction

A capacity controller observes a running system and decides whether to add, retain, or remove resources. Adding resources too late increases queueing delay. Retaining excess resources improves responsiveness but incurs continuing cost. The problem is particularly difficult when new capacity takes time to become usable and requests differ in both CPU and memory requirements.

This report studies **capacity management, or autoscaling**. It does not study which job should be assigned to which node. Job placement is fixed across all policies; the experimental variable is the policy that changes the number of available worker nodes. Nor is the workload held constant over time. Instead, each policy is evaluated against the same recorded arrival sequence for a given trace, including its spikes, ramps, and changes in resource mix.

We examine two model components with different responsibilities. **Jev** is a model provided by TypeSafe AI that returns structured decisions from supplied evidence; here, its output is a capacity action. TypeSafe describes this family as “System One” models, emphasizing their use as decision functions inside software ([TypeSafe AI, 2026](https://typesafe.ai/blog/introducing-system-one-models-and-jev)). **Toto** is a time-series forecasting model developed by Datadog. Published work positions Toto as a foundation model for forecasting observability series ([Toto research paper, 2025](https://arxiv.org/abs/2505.14766)). In this study, Toto forecasts future offered demand, and those forecasts become additional evidence for a controller. The deployed forecasting service is not assumed to be identical to the public model described in that paper.

The study asks three questions:

1. How does Jev compare with classical policies in timely completion and capacity consumption?
2. What changes when Toto forecasts are added to the same Jev decision interface?
3. What inference overhead and operational behavior accompany these decisions?

The contribution is an initial paired empirical comparison with recorded evidence, explicit resource accounting, and deterministic replay. We make no claim that the tested controllers are optimally tuned or that the synthetic workloads establish production performance.

## 2. Control architecture and safety boundary

### 2.1 Separating recommendations from execution

**Reflex** is the Rust library used to define and execute the application's state machine. It accepts proposed actions, checks deterministic guards and invariants, and applies permitted transitions. A model recommendation therefore does not directly modify the cluster.

<figure class="architecture">
<div>Typed system state</div><span>→</span><div>Policy recommendation<br><small>classical or Jev; optional Toto evidence</small></div><span>→</span><div>Deterministic checks</div><span>→</span><div>Permitted transition</div>
<figcaption>Figure 1. Every policy uses the same observation and execution boundary. Forecasting changes the evidence; decision policies change the recommendation; Reflex controls whether the transition can execute.</figcaption>
</figure>

In this application, nodes have four lifecycle states: **off**, **starting**, **ready**, and **draining**. Starting a node makes it ready after 60 seconds. Draining prevents new assignments while existing jobs finish; the node then becomes off. The action set is **hold**, **start one**, **start two**, and **drain one**. A deterministic rule chooses the node to drain, preferring the ready node with the lowest reserved CPU, then memory, then identifier.

The executor enforces a maximum of six active nodes, counts starting and draining nodes against that budget, and retains at least one ready node. Capacity changes must be separated by ten seconds. A drain requires an empty queue and no other drain in progress. At execution time, evidence must be no more than ten seconds old and its lifecycle revision must still match. Forecast-backed proposals additionally require a forecast no more than 30 seconds old. Invariants check resource reservations, node lifecycle consistency, and job accounting.

We use “verified transition” to mean a transition that passes these implemented checks. This is not a formal proof of the library or a guarantee that the selected action is good for latency, cost, or future demand. A legal decision can still be inefficient. The study measures decision quality within this common execution boundary; it does not independently evaluate the completeness of that boundary.

### 2.2 Observations and decisions

Controllers evaluate every ten simulated seconds. The current evidence contains queue length, running-job count, oldest queued-job age, per-node free CPU and memory, node lifecycle and startup progress, time since the last capacity action, the node budget, and currently legal actions. It also includes offered-demand averages over the preceding ten seconds.

Both Jev variants receive 256 seconds of historical offered demand, summarized into 16-second averages. The three series are jobs per second, CPU-seconds of requested work per second, and GiB-seconds of requested work per second. Work estimates use requested resources multiplied by estimated duration. They do not use the actual durations that the simulator will realize. Scenario identifiers, random seeds, future arrivals, and actual job durations are excluded from model evidence.

The Jev policy uses the pinned identifier `jev-1.13.0` and one fixed prompt. It chooses among legal actions; when only one action is legal, that action is selected without an API call. The request asks it to prioritize timely completion and then minimize active node-seconds. Probabilities, confidence, response latency, and token usage are recorded. No outcome-based prompt search or domain calibration experiment is performed, and confidence is not a substitute for the deterministic guards.

### 2.3 Forecast evidence

Toto receives the raw one-second versions of the same three offered-demand series over the preceding 256 seconds. Every 20 simulated seconds, it produces a 120-second forecast with pointwise 10th, 50th, and 90th percentiles. For controller input, forecasts are summarized into ten-second buckets. Negative demand predictions are clamped to zero in decision evidence; raw predictions are retained for forecast-error analysis.

Forecasts are collected once for each trace and shared across its forecast-enabled policies. This prevents different forecast responses from confounding the paired policy comparison. The forecast responses do not independently verify the model version.

## 3. Experimental method

### 3.1 Simulation environment

The simulator advances in one-second ticks. Six homogeneous nodes are available, each with eight CPU units and 16 GiB of memory; two nodes are initially ready. Jobs reserve both requested resources for their full actual execution duration. There is no preemption, migration, CPU sharing, or load-dependent execution slowdown.

Arrivals enter one FIFO queue, capped at 128 jobs. A queued job expires after 60 seconds. Placement considers the oldest queued job and assigns it to the first ready node that has sufficient free CPU and memory. If that job fits nowhere, placement stops for that tick: smaller jobs behind it cannot bypass it. This strict FIFO rule creates head-of-line blocking and makes aggregate free resources an incomplete measure of usable capacity.

Each trace has 256 seconds of synthetic prehistory, five minutes of executed warm-up, and 30 minutes of measured arrivals. After arrivals stop, policies receive at most 120 seconds to finish outstanding work. Resource accounting for each policy stops when its queue and running jobs are empty. Warm-up jobs are excluded from service outcomes, but warm-up inference calls remain in the cost accounting. Fixed-six reaches its full capacity during warm-up.

### 3.2 Request classes and workload patterns

Three independent client classes generate Poisson arrivals. Table 1 gives their base rates and job requirements. Actual duration is the ceiling of estimated duration multiplied by a uniform random value between 0.75 and 1.25. Actual duration is hidden from every policy.

**Table 1. Base request classes. CPU denotes reservable simulator capacity, not measured processor utilization.**

| Class | Arrivals per second | CPU per job | Memory per job | Estimated duration |
|---|---:|---:|---:|---:|
| Small | 0.35 | 1 | 1 GiB | 6 s |
| CPU-heavy | 0.16 | 4 | 2 GiB | 12 s |
| Memory-heavy | 0.14 | 1 | 8 GiB | 18 s |

The moderate load uses these rates multiplied by the pattern in Table 2. Heavy load multiplies all arrival rates by a further 1.5. Pattern times are relative to the start of measured arrivals. All policies receive identical arrivals and actual durations within each trace.

**Table 2. Workload patterns. A multiplier scales the arrival rate of every class.**

| Pattern | Definition |
|---|---|
| Steady | Constant multiplier of 1.0. |
| Spike | Multiplier 0.8, increasing to 2.5 during seconds 600–900, then returning to 0.8. |
| Ramp | Multiplier 0.7 until second 300; linear increase to 2.2 by 900; plateau until 1200; linear decrease to 0.7 by 1650. |
| Waves | Sinusoidal multiplier between 0.6 and 2.0, with a 300-second period. |
| Bursts | Multiplier 3.0 for the first 40 seconds of each 240-second interval; 0.7 otherwise. |
| Resource mix | Constant arrival rates; numerical CPU and memory requests are swapped during seconds 600–1200, remaining within node limits. |

<figure id="workload-patterns"><a href="workload-patterns.png"><img src="workload-patterns.png" alt="Six workload shapes: steady, spike, ramp, waves, bursts, and a CPU/memory resource-mix shift"></a><figcaption>Figure 2. Configured workload patterns over the 30-minute measurement period. Panels a–e show arrival-rate multipliers: 1× corresponds to 0.65 requests/s at moderate load and 0.975 requests/s at heavy load. Panel f normalizes expected CPU work and memory work separately to their own usual levels: swapping job resource requests raises CPU work to 2.12× and lowers memory work to 0.47× while the arrival rate remains unchanged. These are expected rates; realized Poisson arrivals fluctuate around them.</figcaption></figure>

Pattern timing is fixed across seeds. Seeds change the arrivals and actual durations, so this experiment measures variability around these particular patterns rather than a distribution over arbitrary incident schedules.

### 3.3 Policies and tuning

**Fixed 2** keeps the initial two nodes running and never scales. **Fixed 6** expands to six nodes and retains them. These are capacity reference points, not adaptive algorithms or optimal bounds.

The **reactive** controller converts the preceding ten seconds of offered work into a desired node count. If CPU work is C and memory work is M, the unconstrained target is `ceil(max(C / 8, M / 16) / u)`, where u is the target reservation fraction. The target is bounded to one through six nodes. If work is queued, it requests at least one more node than the current ready count, subject to that budget. It accounts for starting nodes when adding capacity and waits before downscaling.

The **EWMA** controller applies the same target calculation to an exponentially weighted moving average of observed demand. Its smoothing coefficient controls how strongly recent demand changes affect the target.

The **HPA-inspired** controller derives a desired count from currently reserved resources relative to a target, with a 10% tolerance and stabilization of downscale recommendations. It also includes queue rescue. Kubernetes HPA similarly relates observed metrics to desired scale, but the production implementation has additional semantics ([Kubernetes documentation](https://kubernetes.io/docs/concepts/workloads/autoscaling/horizontal-pod-autoscale/)). Our controller manages simulated worker capacity, not Kubernetes pods, and is not an implementation-equivalence benchmark against HPA.

The **persistence** policy uses constant forecasts equal to the latest observed demand. Under this implementation's maximum-of-observed-and-forecast rule, it is mathematically identical to reactive control. Its inclusion checks implementation consistency; it is not an independent algorithmic result.

**Toto + reactive** substitutes the maximum of observed demand and the future ten-second mean p90 demand buckets into the reactive target calculation. It retains the no-forecast policy's parameters. **Jev** selects actions directly from current state and history; **Jev + Toto** adds the forecast evidence described in Section 2.3. All nine policies execute through the same Reflex machine.

Classical tuning uses seeds 11–13 on all six patterns and both load levels. The grid spans target fractions 0.6, 0.8, and 0.9; downscale waits of 30 and 120 seconds; and EWMA coefficients 0.2 and 0.6. Parameters irrelevant to a policy produce duplicate configurations. For each family, the selection rule prefers the least capacity among configurations achieving at least 99% mean on-time completion. None reaches that threshold, so the alternative rule selects the highest completion score, breaking ties by capacity consumption. All selected policies use a target of 0.6 and a 120-second downscale wait; EWMA uses coefficient 0.6.

Held-out evaluation uses seeds 1001–1010, giving 6 patterns × 2 loads × 10 seeds = 120 paired traces and nine policy outcomes per trace. Parameters and the Jev prompt are frozen before held-out evaluation. The experiment does not optimize a separate forecast-aware classical policy.

### 3.4 Timing and failure handling

The primary experiment assigns every policy decision and forecast a one-second simulated availability delay. Traffic advances before a proposed action executes, allowing freshness and lifecycle checks to reject outdated proposals. Wall-clock API latency is measured separately. This design compares decision quality under equalized delays; it does not measure a subsecond production control loop.

Jev requests have a five-second deadline and Toto requests an eight-second deadline. There are no automatic inference retries in this runner. Failed Jev evaluations use the reactive fallback and are recorded. Accepted primary results contain ten response-validation failures across 39,781 Jev calls. Forecast-enabled policies initially operate without a forecast until the first response becomes available; those expected cases are counted separately from successful forecast RPCs in the recorded diagnostics.

The original API account exhausted its credits during collection. The original runner continued with fallback on billing failures. All affected or incomplete traces were quarantined and rerun with a funded key, preserving the same workloads, cached forecasts, model identifier, prompt, and parameters. Seventy-five clean complete traces were retained and 45 traces were rerun. The revised runner stops all workers on billing or authorization errors. No accepted primary trace contains such an error; costs from discarded attempts remain in the audit.

### 3.5 Outcomes and statistical analysis

For a measured-period job j, let aⱼ be its arrival time, cⱼ its completion time, and d̂ⱼ its estimated duration. It meets the service-level objective (SLO) when it completes and `cⱼ − aⱼ ≤ d̂ⱼ + 20 seconds`. The denominator includes every offered measured-period job, including rejected and expired jobs. This is a completion deadline, not merely a request-success rate.

Capacity consumption is the integral of the number of active nodes over the measurement and completion periods, expressed as node-hours. Starting and draining nodes count because they consume capacity even when unavailable for new work. Reservation utilization divides resources reserved by running jobs by active resource capacity. It is not an operating-system CPU utilization measurement. Secondary outcomes include completion and rejection counts, completed-job queue waits, capacity changes, guard rejections, and inference latency.

Aggregate results weight all workload traces equally. Thus the reported percentage is a mean of per-trace rates, not a pooled rate over all jobs. Paired differences compare policies on the same trace. Confidence intervals use 10,000 bootstrap resamples of entire seed blocks, retaining all patterns, load levels, and policies within a sampled seed. This preserves dependence from shared random draws across workload cells. The ten seeds are the independent blocks; individual requests are not treated as independent experimental replicates. Intervals are exploratory, without a multiple-comparison adjustment, and do not separately identify model-response variability.

Jev costs use response token counts and the published input price of $0.042 per million tokens, with free output tokens, recorded for `jev-1.13.0` ([TypeSafe model documentation](https://docs.typesafe.ai/models)). Missing-usage calls are excluded from the known estimate. Toto has no available monetary price in this experiment. Any conversion of node-hours to dollars uses the illustrative rate $0.10 per node-hour, not a provider quotation or an actual infrastructure bill.

## 4. Results

### 4.1 Aggregate quality and capacity consumption

Table 3 summarizes all 120 held-out traces. Jev achieves 88.33% mean on-time completion, compared with 86.33% for reactive control and 87.50% for HPA-inspired control. It also consumes more capacity: 3.001 node-hours per trace, versus 2.873 and 2.922. Fixed-six achieves 88.69% using 3.022 node-hours. Therefore, higher SLO success than a scaling baseline does not by itself demonstrate better resource efficiency.

**Table 3. Aggregate outcomes. Intervals are 95% seed-cluster bootstrap intervals. Queue wait is the mean of each run's completed-job p95, not a pooled percentile. Jev cost is the total across all 120 runs of an arm; zero denotes no Jev use, not zero total operating cost.**

| Policy | SLO success | 95% interval | Mean node-hours | Mean run p95 wait | Jev cost |
| --- | --- | --- | --- | --- | --- |
| Fixed 2 | 4.67% | 3.75–5.45% | 1.033 | 59.0 s | $0.00000 |
| Fixed 6 | 88.69% | 87.07–90.03% | 3.022 | 19.1 s | $0.00000 |
| Reactive | 86.33% | 84.55–87.88% | 2.873 | 22.9 s | $0.00000 |
| HPA-inspired | 87.50% | 85.88–88.94% | 2.922 | 20.9 s | $0.00000 |
| EWMA | 84.87% | 83.05–86.50% | 2.760 | 24.4 s | $0.00000 |
| Persistence | 86.33% | 84.55–87.88% | 2.873 | 22.9 s | $0.00000 |
| Toto + reactive | 88.69% | 87.07–90.03% | 3.022 | 19.1 s | $0.00000 |
| Jev | 88.33% | 86.62–89.76% | 3.001 | 19.6 s | $1.44234 |
| Jev + Toto | 88.13% | 86.37–89.63% | 2.971 | 20.0 s | $3.26084 |

<figure><img src="tradeoff.png" alt="Mean node-hours and on-time completion with confidence intervals"><figcaption>Figure 3. Capacity consumption versus on-time completion. The left panel includes the overloaded fixed-two reference; the right expands the other policies. Lower capacity and higher completion are preferable. Overlapping markers may represent equal outcomes.</figcaption></figure>

Jev improves on-time completion over reactive control by 2.00 percentage points, with a paired interval of +1.47 to +2.56 points, while using 4.44% more capacity. Its advantage over HPA-inspired control is +0.83 points, with an interval of +0.33 to +1.34, at 2.69% greater capacity consumption. These results support a tradeoff between quality and resources; the study does not compare these controllers at a matched capacity budget.

### 4.2 Effect of forecast evidence

Adding Toto to Jev changes mean on-time completion from 88.33% to 88.13%. The paired change is −0.19 percentage points, with an exploratory interval of −0.40 to −0.01. Capacity consumption falls by 1.00%, or approximately 1.8 node-minutes per trace. This is a small resource saving accompanied by lower completion quality, rather than an improvement on both dimensions.

**Table 4. Paired differences. A positive SLO change improves completion quality; a negative capacity change saves resources. Brackets contain 95% seed-cluster bootstrap intervals.**

| Comparison | SLO difference | Node-hours difference |
| --- | --- | --- |
| Jev − Reactive | +2.00 pp [+1.47, +2.56] | +0.128 h [+0.115, +0.140] |
| Jev − HPA-inspired | +0.83 pp [+0.33, +1.34] | +0.079 h [+0.062, +0.097] |
| Jev + Toto − Jev | -0.19 pp [-0.40, -0.01] | -0.030 h [-0.050, -0.013] |
| Toto + reactive − Reactive | +2.36 pp [+1.81, +2.91] | +0.149 h [+0.141, +0.159] |
| Jev + Toto − Toto + reactive | -0.56 pp [-0.92, -0.22] | -0.051 h [-0.076, -0.030] |

<figure><img src="ablations.png" alt="Paired differences in SLO success and capacity consumption"><figcaption>Figure 4. Paired policy and forecast comparisons. Confidence intervals describe variability across the evaluated seed blocks conditional on the collected model responses.</figcaption></figure>

Toto + reactive improves on-time completion over reactive control by 2.36 points but consumes approximately the same capacity as fixed-six and achieves the same completion score on every trace. Its conservative p90-based policy largely retains maximum capacity. The fixed-capacity reference is therefore essential: the result does not establish that forecasting anticipated demand usefully relative to simply keeping six nodes ready.

### 4.3 Workload dependence and the fixed-two result

The workload-specific results in Appendix A show that aggregate rankings do not apply uniformly. Under steady traffic, all policies except fixed-two achieve 100% on-time completion at both load levels. On the moderate spike, fixed-six and Toto + reactive achieve 93.47%, Jev 89.91%, and Jev + Toto 88.17%. On heavy waves, both Jev arms achieve 86.96%, slightly above fixed-six at 86.85%. These are descriptive cell means, not independently corrected significance tests.

Fixed-two's aggregate score of 4.67% does not mean almost every job fails to complete. Averaged over traces, 76.74% complete eventually: 4.67% on time and 72.06% late. The remaining 23.26% are rejected or expire. Two nodes provide only 16 CPU and 32 GiB. Realized steady moderate offered work corresponds to about 13.1 CPU and 27.4 GiB of time-averaged demand; steady heavy demand exceeds two-node capacity. Packing restrictions and FIFO head-of-line blocking further reduce usable capacity. Even under steady moderate traffic, the mean per-run median wait is 51.8 seconds, well beyond the approximately 20-second waiting allowance. Fixed-two is consequently an overloaded reference, not a strong competitive baseline.

<figure><img src="timeline.png" alt="Offered CPU and memory work, ready capacity, queue length and starting nodes during one spike trace"><figcaption>Figure 5. A prespecified illustrative trace: moderate spike, seed 1001. Time is relative to the end of warm-up. Offered work is shown as a 30-second mean; ready capacity, queue length, and starting nodes expose the consequences of scaling decisions. This one trace is not representative evidence for every workload.</figcaption></figure>

### 4.4 Forecast error, latency, and inference cost

The primary collection contains 12,600 successful Toto forecast RPCs with no failures. Measured RPC latency is 138.1 ms at the median and 241.0 ms at p95. Table 5 evaluates raw pointwise median forecasts against realized offered work, alongside constant forecasts based on the most recent ten-second mean and median. Forecast errors and intervals overlap across horizons and are descriptive rather than independent trials.

**Table 5. Forecast diagnostics, averaged across workload cells and seeds. MAE denotes mean absolute error. CPU and memory errors have units CPU-seconds/s and GiB-seconds/s; job errors have units jobs/s. Coverage is the fraction within the predicted p10–p90 interval.**

| Series | Horizon | Toto MAE | Mean persistence MAE | Median persistence MAE | Coverage |
| --- | --- | --- | --- | --- | --- |
| jobs | 1–30s | 0.775 | 0.830 | 0.816 | 79.9% |
| jobs | 31–60s | 0.794 | 0.862 | 0.847 | 78.9% |
| jobs | 61–120s | 0.799 | 0.880 | 0.868 | 79.8% |
| cpu | 1–30s | 17.691 | 21.117 | 18.628 | 81.1% |
| cpu | 31–60s | 17.993 | 21.648 | 19.174 | 79.0% |
| cpu | 61–120s | 18.041 | 21.887 | 19.497 | 79.7% |
| memory | 1–30s | 35.975 | 46.844 | 37.970 | 84.1% |
| memory | 31–60s | 35.824 | 47.229 | 38.112 | 83.1% |
| memory | 61–120s | 36.052 | 47.975 | 38.757 | 83.5% |

Toto has lower median-forecast MAE than both persistence references in these summaries. That forecasting advantage does not translate into higher SLO success for Jev in the present control design. Pointwise accuracy, uncertainty representation, and the controller's use of forecasts are distinct parts of the pipeline.

Jev-only uses 19,896 calls and approximately 34.34 million input tokens; Jev + Toto uses 19,885 calls and approximately 77.64 million input tokens. The forecast evidence therefore increases Jev inference cost by a factor of 2.26, despite nearly equal call counts. Median/p95 latency is 182.0/292.2 ms for Jev and 190.3/322.0 ms for Jev + Toto. Costs include warm-up calls.

**Table 6. Known estimated Jev cost by experimental stage. These are usage-based estimates, not invoice totals. Toto monetary cost is unknown and excluded.**

| Stage | Known estimated Jev cost |
| --- | --- |
| Accepted primary study | $4.703177 |
| Discarded interrupted attempts | $0.096510 |
| Measured-latency sensitivity | $0.268218 |
| Preflight | $0.001323 |
| Funded-key validation | $0.000074 |
| Total | $5.069301 |

The total known Jev expense is $5.06930, including $4.70318 for accepted primary results. The audit also records 6,553 billing-declined attempts and 12 other calls without usage across all stages. No charge is inferred for those calls. Consequently, the known total is not a complete monetary bill for the experiment.

### 4.5 Additional diagnostics

A measured-latency sensitivity re-evaluates HPA-inspired, Jev, and Jev + Toto on the six moderate workloads for seed 1001, using new live Jev responses and the recorded forecasts. HPA outcomes are unchanged. Jev gains 1.59 SLO points on the burst trace; the remaining SLO comparisons are unchanged. Because this diagnostic changes model responses as well as latency, it cannot attribute that difference to latency alone. One-second ticks also hide most subsecond RPC differences.

A second diagnostic provides exact future offered demand to the forecast-enabled reactive policy on the same six moderate traces. It preserves SLO success while reducing capacity relative to the Toto-backed policy on each trace. This suggests scope to improve forecast use, but the diagnostic is neither deployable nor an optimal-control bound. Full numerical comparisons are retained in the [diagnostic supplement](SUPPLEMENT.html).

## 5. Discussion and limitations

The first lesson is methodological: **service quality and resource consumption must be interpreted together**. Jev's higher completion score than the reactive and HPA-inspired baselines is accompanied by more capacity. The near-maximum capacity of both Jev arms, and the strong fixed-six result, make it premature to conclude that model-guided decisions are more efficient.

Second, forecast accuracy is not control utility. Sparse one-second work series, pointwise uncertainty bounds, and a policy that takes the maximum of p90 demand can favor sustained high capacity. Averaging pointwise medians does not produce the expected aggregate demand over a startup interval. The experiment also supplies Toto with only 256 seconds of context, shorter than the 300-second wave period. These are properties of this integration, not limits on the best possible Toto-based controller.

Third, the controller cannot repair an inefficient placement rule. Strict FIFO and nonpreemptive two-resource reservations create blocking and fragmentation. Fixed-two is already overloaded under much of the workload, and fixed-six nearly matches the best observed SLO. Intermediate fixed capacities, lighter loads, alternative placement rules, and comparisons at matched resource budgets would better resolve where adaptation adds value. They were not evaluated here.

Several further limits constrain interpretation. Only ten independent seed blocks and one live model execution per arm per trace are available. Workload timings are fixed, tuning covers a small parameter grid, and no prompt search or forecast-specific classical tuning was performed. The study does not include network failures, telemetry delay, shared-CPU contention, migration, or realistic infrastructure pricing. Equalized one-second timing removes much of the deployment-latency question. The forecast server's version is not verified by its response. A funded-key change divides collection into two periods, although the requested Jev version and experimental inputs remain fixed. Finally, no calibration analysis or ablation of the Reflex executor itself is included.

These limits motivate a subsequent experiment with intermediate capacity references, repeated model responses, startup-horizon aggregate forecasts, and a preregistered resource-quality objective. Such extensions would test hypotheses suggested by this study rather than retroactively changing its conclusions.

## 6. Conclusion

In this synthetic autoscaling experiment, Jev improves on-time completion over tuned reactive and HPA-inspired controllers while consuming more capacity. Adding Toto forecasts produces a small capacity saving, lower on-time completion, and higher Jev inference cost. A fixed-six reference performs strongly, and Toto + reactive behaves much like that reference. The evidence therefore supports a measured quality-resource tradeoff, not a general superiority claim for model-guided control or for forecasting. Reflex provides a common deterministic execution boundary; whether recommendations within that boundary are useful remains an empirical question.

## Reproducibility and data availability

All 120 paired traces and 1,080 policy outcomes passed exact no-network replay. An independent validator checked job identities across policies, outcome conservation, SLO arithmetic, historical-only forecast inputs, forecast shapes, cost arithmetic, capacity bounds, and persistence/reactive equivalence. Replay reproduces recorded costs as metadata but incurs no new inference calls.

The accompanying local artifact contains the [per-run CSV](runs.csv), [summary statistics](summary.json), [validation result](validation.json), [source hashes and replay record](REPRODUCIBILITY.json), [protocol and analysis notes](PROTOCOL.md), and [full cost audit](supplement.json). The [interactive report view](explorer.html) retains the original charts and diagnostics. Raw workloads, decisions, forecasts, and job records reside in the study output directory; they have not been published as a public dataset. Source instructions are in `studies/capacity/README.md`. This document is a technical report, not a peer-reviewed publication.

## Appendix A. Workload-specific completion scores

**Table A1. Mean SLO success across ten seeds for each workload and load level. Bold values identify the highest mean within a row; ties are included. Bold denotes an observed maximum, not statistical significance.**

| Workload | Load | Fixed 2 | Fixed 6 | Reactive | HPA-inspired | EWMA | Persistence | Toto + reactive | Jev | Jev + Toto |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| steady | moderate | 1.12% | **100.00%** | **100.00%** | **100.00%** | **100.00%** | **100.00%** | **100.00%** | **100.00%** | **100.00%** |
| steady | heavy | 0.00% | **100.00%** | **100.00%** | **100.00%** | **100.00%** | **100.00%** | **100.00%** | **100.00%** | **100.00%** |
| spike | moderate | 26.74% | **93.47%** | 85.81% | 86.83% | 85.59% | 85.81% | **93.47%** | 89.91% | 88.17% |
| spike | heavy | 0.00% | **64.83%** | 63.90% | 64.11% | 63.57% | 63.90% | **64.83%** | **64.83%** | 64.72% |
| ramp | moderate | 13.32% | **100.00%** | 99.97% | **100.00%** | 99.78% | 99.97% | **100.00%** | **100.00%** | 99.94% |
| ramp | heavy | 0.02% | 43.48% | 40.93% | 43.45% | 40.68% | 40.93% | 43.48% | 43.48% | **43.61%** |
| waves | moderate | 1.43% | **100.00%** | **100.00%** | 99.99% | 99.56% | **100.00%** | **100.00%** | **100.00%** | **100.00%** |
| waves | heavy | 0.00% | 86.85% | 83.77% | 86.66% | 77.64% | 83.77% | 86.85% | **86.96%** | **86.96%** |
| bursts | moderate | 11.86% | **99.18%** | 94.79% | 96.25% | 94.29% | 94.79% | **99.18%** | 98.70% | 98.32% |
| bursts | heavy | 0.33% | **82.77%** | 77.07% | 79.18% | 69.62% | 77.07% | **82.77%** | 82.30% | 82.20% |
| mix | moderate | 1.25% | **100.00%** | 99.98% | 99.80% | 99.81% | 99.98% | **100.00%** | **100.00%** | 99.97% |
| mix | heavy | 0.00% | **93.71%** | 89.71% | **93.71%** | 87.88% | 89.71% | **93.71%** | **93.71%** | **93.71%** |

## Appendix B. Fixed Jev instruction

Both Jev arms use the following instruction; only their supplied evidence differs.

> Choose a capacity action. Primary objective: complete offered jobs within estimated duration plus 20 seconds of arrival; minimize active node-seconds subject to this objective. Rejected jobs fail the objective. Each node has 8 CPU and 16 GiB, jobs reserve both until completion. FIFO first-fit placement is fixed. Starting nodes cost resources but cannot accept jobs until startup_s; draining nodes finish existing jobs and accept no new jobs. Account for starting capacity. Recent_history contains offered jobs/s, CPU-seconds/s and GiB-seconds/s averaged in time buckets, oldest first. Forecasts, when present, are uncertain pointwise p10/p50/p90 demand estimates, not guarantees. Anticipate startup delays, avoid unnecessary starts and oscillation, keep at least one ready node. Select only a legal choice. No future arrivals or true job durations are provided.

## References

1. TypeSafe AI. [Introducing System One Models & Jev](https://typesafe.ai/blog/introducing-system-one-models-and-jev). September 15, 2026. Used for terminology and the intended structured-decision interface, not as evidence of this experiment's results.
2. TypeSafe AI. [Models](https://docs.typesafe.ai/models). Pricing and model identifiers; study price recorded September 20, 2026.
3. [This Time is Different: An Observability Perspective on Time Series Foundation Models](https://arxiv.org/abs/2505.14766). 2025. Background on Toto; not proof of the identity or performance of the deployment used here.
4. Kubernetes contributors. [Horizontal Pod Autoscaling](https://kubernetes.io/docs/concepts/workloads/autoscaling/horizontal-pod-autoscale/). Algorithm documentation; the study's HPA-inspired policy is a simplified, modified baseline.
