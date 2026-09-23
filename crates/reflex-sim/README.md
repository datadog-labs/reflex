# Circuit Lab

For applications that **publish telemetry to Datadog and query it back as decision state**, see the [Datadog control-loop walkthrough](DATADOG.md). It covers circuit breaking and scheduling through the normal playground CLI.


Circuit Lab replays the same offered traffic against independent circuit-breaker policies. Each downstream has a worker pool and a bounded queue. Sending more traffic into a degraded service builds pressure, raises failure probability, and can prolong the incident.

Canned reports include an unprotected reference and a deterministic threshold breaker implemented with Reflex. They make no inference calls and need no API key. Incident Playground also supports live Jev recommendations through the TypeSafe client and Reflex controller.

## Run

From the repository root:

```sh
npm ci --prefix crates/reflex-sim/ui
npm run build --prefix crates/reflex-sim/ui
cargo run -p reflex-sim --locked
```

This runs all six scenarios with seed 42 and opens `output/circuit-lab/index.html` in your browser. The HTML embeds its data, JavaScript, and CSS, so it works offline and can be shared as one file. `results.json` contains the scenario definitions, summaries, snapshots, request lifecycles, and transition logs.

```sh
# Explore one scenario with a different seed.
cargo run -p reflex-sim --locked -- --scenario slowdown --seed 7

# Generate artifacts without opening a browser, for CI or a remote terminal.
cargo run -p reflex-sim --locked -- --no-open --output output/my-experiment

# List presets, or choose which registered algorithms run.
cargo run -p reflex-sim --locked -- --list-scenarios
cargo run -p reflex-sim --locked -- --algorithms threshold,unprotected

# Edit a complete scenario definition and run it.
cargo run -p reflex-sim --locked -- --scenario-file crates/reflex-sim/scenarios/slowdown.json
```

Each run replaces `index.html` and `results.json` in the selected output directory. Choose separate directories to retain several seeds. The first selected algorithm is the dashboard's reference for useful-request deltas.

## Incident Playground

```sh
cargo run -p reflex-sim --locked -- --playground
```

The CLI starts a server on `http://127.0.0.1:8742/` and opens the app. Leave the CLI running while you play. Use `--no-open` to start only the server, `--port 8743` to choose another port (or `--port 0` for an available port), and `--seed 7` to choose the traffic seed. Press Ctrl+C to stop. All browser tabs on that server share one session.

1. Start traffic and select Catalog, Payments, or Search on the map.
2. Toggle **Slowdown** (6× processing time), **Error storm** (85% additional error probability), or **Traffic surge** (4× traffic). Faults can be combined and applied while paused.
3. Watch the queue, stress, and circuit gate. Reflex's actual accepted transitions appear in the incident log and selected-service panel.
4. Repair faults to restore baseline conditions. Existing queued and timed-out work remains; pressure recovers as work drains.
5. Pause, advance one second, or change playback speed. Inspect a request to see a snapshot of its client outcome and downstream work.
6. Replay reproduces the same seed and timestamped fault edits up to the current playhead, then pauses. Fault editing is disabled during replay. Reset starts a fresh, paused session with the same seed.

**Export incident** saves the full offered request trace, fault edits, snapshots, and transitions under `incident`, plus `decisions`, the ordered `timeline`, selected `policy`, and inference status. The export describes the current state: unfinished requests have nullable outcomes. Sessions stop at 180 simulated seconds without draining outstanding work. The fault log is limited to 500 edits; reset starts a new incident.

The map samples request particles to stay readable. Queued packet stacks show up to 16 requests; the adjacent queue count is exact. Worker, queue, stress, circuit, and outcome counters come from the Rust engine, refreshed approximately five times per second. A three-minute clock is simulated time: the engine may run slower on a busy machine. The circuit-breaker playground uses Jev + Reflex. Set a TypeSafe API key before starting it. The offline report includes the classical threshold breaker for comparisons.

The incremental engine in `src/engine/live.rs` shares request processing, completion timing, deadlines, stress integration, and the actual Reflex executor with the report engine. It generates a maximum-rate Poisson candidate stream and thins it at each service's current rate. Separate random draws determine acceptance, work, and errors. A traffic change cannot redraw earlier requests or alter another service's offered traffic. Replay uses the original order of fault edits, including edits at the same paused timestamp. `src/playground.rs` provides playback and a loopback-only HTTP API; its clock advances the simulation in small increments independently of browser polling.

The playground's thinning stream differs from the canned report generator, so the same seed across these two modes does not identify the same offered trace. Compare an incident with its replay, or use Circuit Lab's shared trace when comparing policies. A live request trace has no finalized fingerprint; the exported model version, seed, ordered fault log, and explicit requests provide reproducibility.

## Live Jev judgments

Set `TYPESAFE_API_KEY` in the server environment, then launch:

```sh
cargo run -p reflex-sim --locked -- --playground --policy jev
```

The circuit-breaker playground supports only Jev + Reflex and requires `TYPESAFE_API_KEY`. It starts paused; Reset starts a fresh incident. The key stays on the server and is not included in browser responses or incident exports. The server sends only synthetic, client-visible observations to TypeSafe.

`--jev-model jev-1.13.0` selects the model. The playgrounds have no evaluation-count cap: evaluations continue at the existing cadence until the run ends. Pause stops new evaluations, and replay makes no API calls. The dashboard shows the evaluation count and estimated cost.

The **Jev cost so far** panel estimates USD from actual provider-reported input tokens. Jev 1.13 costs $0.042 per million input tokens and output is free ([TypeSafe model pricing](https://docs.typesafe.ai/models), verified September 20, 2026). Cost is accumulated when the HTTP result arrives, including while the simulation is paused and even if guards subsequently reject the decision. The total covers this server process: it survives Reset, and replay does not count recorded usage again. Restarting the server clears it. Exports include the same cumulative cost status.

Expand the panel to see input/output token totals and coverage. Pending, failed, or cancelled calls without usage are excluded and counted explicitly. Resolved models other than `jev-1.13.0` have no configured rate and are also excluded. A partial estimate is labeled; this metric is not an account balance or an invoice.

The system map shows traffic moving toward each service, busy worker indicators, and stacked queue blocks (up to 24, alongside exact queue counts). Circuit status dots sit directly on the traffic paths: **green / Closed** admits normal traffic, **red / Open** blocks new requests, and **yellow / Probe** (half-open) permits one recovery request. Labels and a legend explain the colors; clicking a dot selects its service. **No change** retains the current state; it is not a fourth state. Probe success closes automatically, and probe failure reopens. The inspector and incident log show decisions and transitions.

The instantiated `TypeSafeClient` is injected into `LiveEvaluator`, which builds a typed task and uses `TypeSafeJudge` through `Controller::evaluate`. Every evaluation gets its own adapter so a failed call cannot accidentally display diagnostics from a previous success.

Jev receives circuit phase and revision, one-second and five-second response windows, error/timeout counts, P95 latency, cooldown eligibility, the client timeout, and currently legal actions. It never receives fault controls, server queue depth, server stress, random draws, or future events. It recommends one of:

- **Open**: block further arrivals to a closed circuit.
- **Probe**: enter half-open and reserve capacity for one recovery probe.
- **NoChange**: retain the circuit state, including abstaining when evidence is insufficient.

The Reflex executor checks the recommendation against current state. Opening requires 10 responses in the current five-second window, but it does not duplicate the threshold policy's 50% failure heuristic. Probing requires a three-second cooldown. Responses from earlier circuit generations cannot close a newer circuit. Five consecutive successful probes close the circuit, with only one probe in flight at a time. An error or timeout reopens it and resets the success count. Jev cannot directly force the circuit closed.

Recommendations from an earlier circuit revision or more than five simulated seconds ago are rejected. Evaluation errors go through explicit executor `evaluation_error` rows that retain the current phase. There is no silent switch to the threshold algorithm on provider failure.

The UI separates the selected option's probability (**choice score**) from provider **confidence**, and shows “Not supplied” for missing confidence. Neither is treated as a probability of operational success. This example preserves these signals for inspection and does not enforce a minimum confidence floor. The judgment inspector shows exact sent evidence, provider model, usage, latency, returned scores, and the guard outcome, including rejected recommendations and API errors.

Inference runs outside the simulation lock. There is at most one request in flight, at most one dispatch per wall-clock second, and at most one evaluation per downstream per three simulated seconds. The client/controller deadline is two wall-clock seconds and automatic HTTP retries are disabled, making the displayed call count a count of dispatched evaluations. Fault controls, traffic, and request deadlines continue while the provider is pending. A response is checked on the next simulation tick. While paused, it waits without changing the circuit; stepping or resuming permits application. Reset, policy changes, and replay cancel outstanding evaluations.

Model latency therefore has a visible effect: more traffic can arrive between observation and execution. Faster playback increases the simulated age of the same network response and can cause freshness rejection. This interactive timing model is useful for exploration, but it is not a controlled latency benchmark between algorithms.

Replay reuses the recorded provider responses at their original simulated application times, interleaved with fault edits in their original order, and runs the guards again. It verifies that guard status and destination phase match. It neither reruns Jev nor treats fresh model output as deterministic. All applied response data is retained in the export; a response still pending when replay starts is cancelled and has no recorded effect to reproduce.

## Explore the report

Select a scenario, then click a phase or scrub the replay timeline. Play advances simulated time; it does not rerun the simulation. Filter by downstream to compare useful throughput, queue depth, stress, and client failures. The environment panel shows prescribed traffic, service time, and fault probabilities at the cursor.

Result cards summarize the full offered cohort. Worker activity and stress use periodic snapshots, while circuit transitions and the request journal use exact event times. A request that times out can still appear as queued or running downstream. The journal only reveals outcomes that have happened by the cursor; the JSON export contains complete lifecycles.

## Canned scenarios

Every preset lasts 90 simulated seconds and uses Catalog, Payments, and Search downstreams with independent worker pools and breakers.

| ID | Disturbance |
| --- | --- |
| `healthy` | Normal traffic with a small background error rate |
| `slowdown` | Payments takes 6× longer from 20–45s, with additional errors |
| `error-storm` | Search has 85% additional error probability and 1.5× service time from 20–45s |
| `traffic-surge` | All downstreams receive 4× traffic from 20–45s |
| `brief-spike` | Payments has a two-second traffic, latency, and error spike |
| `flapping` | Payments alternates five-second fault and recovery periods from 20–50s |

Preset definitions live in `src/scenario.rs`. The editable [slowdown JSON](scenarios/slowdown.json) uses the same public `Scenario` schema. Times are milliseconds; rates are offered requests per second. Phases must be contiguous, cover the full duration, and name existing services. Each change replaces that phase's multipliers for one service; omitted services retain their baseline settings.

Inputs are validated before simulation. Limits include 1–8 services, 128 workers and 1,024 queued requests per service, a ten-minute arrival window, and 200,000 offered requests per trace. Manually constructed traces additionally require positive work of at most 60 seconds per request and random error draws in `[0, 1)`.

## Simulation contract

The engine advances directly between arrivals, completions, deadlines, environment boundaries, and chart samples. It uses simulated milliseconds throughout; no wall-clock sleeps approximate service time. At equal times, environment changes precede completions, then deadlines, arrivals, and sampling. A completion exactly on its deadline succeeds or errors without also timing out.

A seeded generator creates Poisson arrivals, nominal work requirements uniformly distributed over 0.65–1.35 times each service's configured work, and one error draw per request. Every algorithm receives the same immutable trace. Each downstream processes one request per worker and queues excess work FIFO. A full queue returns an immediate downstream error, recorded separately from circuit shedding. Prescribed latency multipliers slow work already in progress as well as newly admitted work.

Pressure is modeled as a continuous stress value between zero and one:

```text
target = clamp(((active + queued) / workers - 0.75) / 2, 0, 1)
stress(t + dt) = target + (stress(t) - target) * exp(-dt / tau)
```

The build and recovery time constants are configurable. Recovery defaults to slower than buildup. Stress increases failure probability; queueing and prescribed slowdowns increase latency. At completion:

```text
fault = 1 - (1 - baseline_error) * (1 - phase_error)
p_error = 1 - (1 - peak_fault) * (1 - stress_error_factor * peak_stress^2)
```

Peaks are measured while the request is running. Stress is integrated analytically between events. Successful and failed executions both consume worker time. More admitted traffic increases outstanding work and therefore stress; shedding allows queued work and stress to drain.

A client deadline does not cancel downstream work, including work still queued. Each request has exactly one client outcome: success, downstream error, timeout, or circuit shed. Late success is not counted as useful success. After the arrival horizon, prescribed faults end and remaining work drains under baseline conditions. Summary metrics cover the entire offered cohort; charts stop at the horizon.

Successful latency P95 includes only successes. Admitted-client P95 in JSON includes errors and timeouts, excluding shed requests. Unsuccessful worker time counts all work for requests without client success; post-timeout worker time measures the subset performed after deadlines. Aggregate peak queue is the largest individual downstream queue, and aggregate stress is the maximum downstream stress. Open and half-open durations sum across downstreams within the arrival window.

The trace fingerprint identifies the scenario, model version, seed, and generated requests using FNV-1a; it is a reproducibility aid, not a cryptographic integrity check. This is a synthetic model, not a calibrated production forecast. A single seed is not sufficient to rank algorithms statistically. There are no retries, cancellation, shared worker pools, or network transport simulation yet.

## Policies and Reflex

The `threshold` policy keeps a five-second response window. At least 20 responses and a failure ratio of 50% open the circuit. Errors and timeouts count as failures. After three seconds, the next arrival may reserve one half-open probe. A successful probe closes the circuit; a failed or timed-out probe reopens it. Responses from an earlier circuit generation cannot close a newer one.

`src/policy.rs` declares those transitions, guards, and invariants with `reflex::state_machine!` and executes them through `StateMachineExecutor`. Callback inputs carry simulated timestamps; hooks do not consult wall time.

To add an algorithm, implement `Policy` and register a `PolicyFactory` in `builtins()` for CLI selection:

```rust
pub trait Policy: Send {
    fn phase(&self) -> CircuitPhase;
    fn admit(&mut self, context: AdmissionContext) -> PolicyFuture<'_, Admission>;
    fn observe(&mut self, observation: Observation) -> PolicyFuture<'_, ()>;
}
```

The factory creates a fresh instance for each downstream in each run. Admission returns `Allow { generation, probe }` or `Shed`. The engine echoes that generation when reporting the request's client outcome. Observations include simulated time, request ID, latency, and outcome. Policies have no access to future faults, random draws, server queues, or stress.

Library callers can pass any factory directly to `simulate(&scenario, &trace, factory).await`, then package its `Run` in a `Report`. The same report supports additional algorithms without changing the simulation engine. See `Unprotected` for the smallest implementation and `ThresholdPolicy` for a Reflex-backed one.

Callbacks currently run at admission and client completion and are awaited before simulation advances. Async callbacks permit the SDK's executor, but wall-clock inference duration is not a simulated delay. The playground adds explicit asynchronous scheduling around the engine for Jev; it records simulated application times and real provider latency. The canned report CLI still runs only deterministic registered policies.

## Validate

```sh
cargo test -p reflex-sim --locked
cargo clippy -p reflex-sim --all-targets --locked -- -D warnings
```

Tests check exact FIFO timings, deadline ties, late completion, queued timeouts, fault boundaries, queue overflow, load-induced errors and recovery, work accounting, deterministic replay, circuit generations and probes, custom scenario validation, CLI behavior, safe standalone report serialization, live fault recovery, replay at different clock granularities, and HTTP controls, Jev legality and freshness guards, provider failures, simulation time limits, pending-request cancellation, and replay without additional provider calls.

## Resource Scheduler playground

Open the **Resource Scheduler** tab in the same playground, or visit `/scheduler`. Each scenario keeps a separate run; using the scenario tabs pauses the one you leave. The scheduler uses live Jev by default when `TYPESAFE_API_KEY` is set; otherwise it starts with Best Fit. First Fit and Best Fit remain selectable with a key configured.

Start with three clients and add or remove clients to keep between one and eight. Live mode starts each client at 1 job/s with a 2-second duration. Their CPU/memory requests are 1/2, 2/4, and 3/6 CPU/GiB, for about one-third of the pool’s capacity at steady state. Increase a client’s rate, job size, or duration to create contention. Canned incident workloads retain their own settings. Each client has an enabled flag, a finite nonnegative whole-number arrival rate in jobs/s (no configured maximum), CPU per job (1–32), memory per job (1–64 GiB), and estimated duration (1–30 seconds). Apply changes to update future arrivals. Pausing/removing a source does not cancel its queued or running jobs. Reset retains client configurations and cumulative inference cost.

Four nodes have 4/8, 8/16, 8/16, and 16/32 CPU/GiB capacity. Jobs run on one node without preemption. FIFO is enforced within each client. Jev chooses among the oldest fitting requests from different clients; a blocked head holds only its own client queue. Oversized arrivals are rejected immediately, and the queue is bounded at 128 jobs. The topology view connects client sources to a shared queue, the scheduler, and four illustrated nodes. Moving dots reflect observed arrivals and accepted placements; colors identify clients and dot size reflects CPU requirements. Queue blocks and per-node running-job blocks expose buildup, alongside exact counts and CPU/memory meters. Click a client to edit its settings, the queue to inspect waiting jobs, or a node to inspect its running jobs. The dashboard also shows completed/rejected counts, wait time, occupancy history, and a placement journal with exact Jev evidence and results. The cost panel covers scheduler evaluations only, separately from the circuit-breaker tab.

Arrivals are evenly spaced at each configured rate (period rounded to milliseconds at rates up to 1,000/s; higher rates retain fractional deadlines and can generate multiple arrivals in one millisecond); editing a source schedules its next arrival using the new rate. Actual runtimes have deterministic ±20% variation derived from seed, client ID and that client's arrival sequence. Completions and arrivals are processed chronologically, with completions first on a tie. Baseline placements occur immediately at event boundaries. The clock stops at 180 seconds without draining unfinished work.

The web playground permits a scheduler evaluation every 200 ms, with one evaluation in flight, so healthy inference can keep up with the default 3 jobs/s at 1× speed. Slow or failed inference, deliberate deferral, and faster playback can still build a queue.

Jev receives eligible client queue heads with current priorities and waiting times, current available CPU/memory, running jobs' estimated remaining durations, per-client performance, and up to eight other queued jobs. It chooses a request and node or Defer; legal choices identify currently feasible placements. In Datadog mode, the same current application state is always supplied. Valid Datadog snapshots are attached as optional delayed context; missing, stale, or incomplete telemetry does not block evaluation or placement. Evidence excludes actual future completion times. A single aggregate Reflex state machine commits job lifecycle and resource accounting together. Guards require that the selected request is still its client's queue head, priorities have not changed, the aging rule is honored, capacity fits in both dimensions, and evidence is at most five simulated seconds old. Invariants recompute node reservations from running jobs and forbid overcommit. Completed jobs retain their historical node ID but no active reservation. Rejected recommendations and evaluation errors retain the queued job.

The injected `scheduler::judge::Evaluator` uses the standalone TypeSafe client, `TypeSafeJudge`, and `Controller`. Inference runs outside the session lock, at most one request at a time with 200 ms between dispatches in the web playground, using the CLI's model until the simulation ends. Results arriving while paused are metered immediately but applied only when the clock resumes or steps. Policy changes and Reset cancel pending inference. Jev placement throughput depends on evaluation cadence and response latency; do not interpret this interactive comparison with instantaneous baselines as a controlled latency benchmark.

Both baselines select the highest-priority fitting client head, with arrival-order ties and the same aging rule. First Fit selects the first fitting node; Best Fit minimizes the sum of normalized CPU and memory left after placement. Both use the same executor guards and invariants. The scheduler export preserves full job history, decisions and evidence, client configuration edits, occupancy history and cost. Scheduler replay/import is not implemented.

### Live per-client lag

The scheduler includes two time-series charts covering the last 60 simulated seconds: oldest currently queued request age, and mean queue wait for requests that started in the preceding ten seconds. An empty queue has zero oldest wait. A window without starts is a gap in the start-lag chart, not a zero-latency observation. Running time is excluded from both metrics. The legend can hide/show multiple clients; colors match the topology, and dashed timeline markers record live priority edits. The accompanying table shows exact current waits, queue depth and recent start counts, including work left by removed clients. These are simulator visualization measurements, independent of the configured Jev evidence source.

### Live client priorities

Click a client and change **Live priority** to Normal, High or Critical. This is an immediate control, separate from workload edits: it changes queued and future requests, preserves arrival timing, and cancels pending judgments without restarting the run or interrupting running jobs. Jev treats priority as a preference among feasible client heads. After 30 simulated seconds of waiting, the oldest feasible overdue head becomes the only eligible request, and Defer is excluded. When aging leaves exactly one legal placement, the executor applies it directly without a Jev call. This protects aged requests at the next successful placement opportunity; it does not guarantee a 30-second wait under overload, insufficient capacity or inference failure.

The topology and queued cards show priority badges. The client inspector reports queued count, completions, completed jobs per simulated second since run start, mean/p95 wait for started jobs, and oldest current queue wait. Priorities remain attached to queued work from removed clients. Sandbox resets retain priority settings; selecting/resetting a canned scenario restores its default clients at Normal priority. Priorities are live controls, not part of a canned schedule.

## Forecasting

Toto forecasting is integrated into circuit breaking and scheduling. See the [forecasting guide](FORECASTING.md) for setup, observation series, history windows, uncertainty, and fallback behavior. The standalone capacity tab is retired; its underlying module remains available for research and recorded study replay.


## Canned incident runs

Each playground has a Scenario selector and a **Run scenario** button. Selecting a preset resets that run; Run scenario resets and starts it. Reset repeats the selected preset. Forecast on/off and the active policy are retained. Scheduled changes are simulation events, so stepping and accelerated playback do not skip them. The schedules and preset names are not sent to Jev or Toto as evidence.

| Playground | Preset | Scheduled changes (local simulation time) |
| --- | --- | --- |
| Circuit breaker | Slowdown + traffic surge | At 75s, Payments becomes 6× slower with 4× traffic; normal conditions return at 120s. |
| Circuit breaker | Recurring error storms | Search receives an 85% injected error probability during 75–100s and 125–150s. |
| Scheduler | Traffic burst | Three clients start at 2 jobs/s each, rise to 6 jobs/s at 75s, then ease to 1 job/s at 115s. |
| Scheduler | CPU / memory mix shift | At 75s Client 2 changes to 8 CPU / 2 GiB jobs and Client 3 to 2 CPU / 24 GiB jobs. Original sizes return at 125s; rates stay fixed. |

Scheduler presets restore three known client profiles. Sandbox mode retains custom client settings on reset. Manual controls remain available during presets; later scheduled changes still apply. Circuit-breaker replay records the actual fault edits, including scheduled changes.

For Datadog-backed circuit breaking and scheduling, incident preset times are multiplied by three (the repeating demand cycle remains 60 seconds), allowing remote history to warm up before the first change. Their descriptions display the actual times.

## Local Toto forecasts

Run the optional [Python Toto service](../../integrations/toto/README.md), then add
`--toto-url http://127.0.0.1:8765` to the playground command. The same provider
serves Circuit Breaker and Resource Scheduler using local observations
or queried Datadog history. Forecast switches and comparison charts are available
in each simulation.
