use super::{
    engine::{Data, Job, JobPhase},
    Policy,
};
use opentelemetry::{
    metrics::{Counter, Histogram, Meter},
    KeyValue,
};
use std::{
    collections::BTreeSet,
    sync::{Arc, Mutex},
};

pub(super) fn policy(policy: Policy) -> &'static str {
    match policy {
        Policy::Jev => "jev",
        Policy::FirstFit => "first_fit",
        Policy::BestFit => "best_fit",
    }
}
pub(super) fn client(id: u64) -> String {
    format!("client_{}", id + 1)
}
pub(super) fn node(index: usize) -> &'static str {
    ["node_a", "node_b", "node_c", "node_d"][index]
}
const GIB: u64 = 1 << 30;
#[derive(Default)]
struct Snapshot {
    queues: Vec<(Vec<KeyValue>, u64, f64)>,
    nodes: Vec<(Vec<KeyValue>, [u64; 5])>,
}
pub(super) struct Telemetry {
    policy: &'static str,
    run: Option<String>,
    known_clients: BTreeSet<u64>,
    seen: Vec<JobPhase>,
    snapshot: Arc<Mutex<Snapshot>>,
    jobs: Counter<u64>,
    placements: Counter<u64>,
    duration: Histogram<f64>,
    run_duration: Histogram<f64>,
    wait: Histogram<f64>,
    cpu: Histogram<u64>,
    memory: Histogram<u64>,
}
impl Telemetry {
    pub fn new(meter: Meter, selection: Policy, run: Option<String>) -> Self {
        let snapshot = Arc::new(Mutex::new(Snapshot::default()));
        let weak = Arc::downgrade(&snapshot);
        meter
            .u64_observable_gauge("scheduler.queue.depth")
            .with_unit("{job}")
            .with_callback(move |observer| {
                if let Some(s) = weak.upgrade() {
                    for (tags, depth, _) in &s.lock().unwrap_or_else(|e| e.into_inner()).queues {
                        observer.observe(*depth, tags);
                    }
                }
            })
            .build();
        let weak = Arc::downgrade(&snapshot);
        meter
            .f64_observable_gauge("scheduler.queue.oldest_age")
            .with_unit("s")
            .with_description(
                "Age of oldest queued job in simulated seconds; zero for an empty queue",
            )
            .with_callback(move |observer| {
                if let Some(s) = weak.upgrade() {
                    for (tags, _, age) in &s.lock().unwrap_or_else(|e| e.into_inner()).queues {
                        observer.observe(*age, tags);
                    }
                }
            })
            .build();
        for (index, name, unit) in [
            (0, "scheduler.node.jobs.running", "{job}"),
            (1, "scheduler.node.cpu.capacity", "{cpu}"),
            (2, "scheduler.node.cpu.reserved", "{cpu}"),
            (3, "scheduler.node.memory.capacity", "By"),
            (4, "scheduler.node.memory.reserved", "By"),
        ] {
            let weak = Arc::downgrade(&snapshot);
            meter
                .u64_observable_gauge(name)
                .with_unit(unit)
                .with_callback(move |observer| {
                    if let Some(s) = weak.upgrade() {
                        for (tags, values) in &s.lock().unwrap_or_else(|e| e.into_inner()).nodes {
                            observer.observe(values[index], tags);
                        }
                    }
                })
                .build();
        }
        Self {
            policy: policy(selection),
            run,
            known_clients: BTreeSet::new(),
            seen: vec![],
            snapshot,
            jobs: meter
                .u64_counter("scheduler.jobs")
                .with_unit("{job}")
                .with_description("Jobs recorded once on completion or rejection")
                .build(),
            placements: meter
                .u64_counter("scheduler.placements")
                .with_unit("{attempt}")
                .build(),
            duration: duration(&meter, "scheduler.job.duration"),
            run_duration: duration(&meter, "scheduler.job.run.duration"),
            wait: duration(&meter, "scheduler.queue.wait"),
            cpu: meter
                .u64_histogram("scheduler.job.cpu")
                .with_unit("{cpu}")
                .with_boundaries(vec![1.0, 2.0, 4.0, 8.0, 16.0, 32.0])
                .build(),
            memory: meter
                .u64_histogram("scheduler.job.memory")
                .with_unit("By")
                .with_boundaries([1, 2, 4, 8, 16, 32, 64].map(|n| (n * GIB) as f64).to_vec())
                .build(),
        }
    }
    fn scoped(&self, mut tags: Vec<KeyValue>) -> Vec<KeyValue> {
        if let Some(run) = &self.run {
            tags.push(KeyValue::new("simulation_run", run.clone()));
        }
        tags
    }
    fn tags(&self, job: &Job, include_node: bool) -> Vec<KeyValue> {
        let mut tags = vec![
            KeyValue::new("policy", self.policy),
            KeyValue::new("client", client(job.client)),
        ];
        if include_node {
            if let Some(index) = job.node {
                tags.push(KeyValue::new("node", node(index)));
            }
        }
        self.scoped(tags)
    }
    // Observe committed state after every arrival, clock event, and placement. An unchanged
    // phase cannot recount an event. A fresh instance owns each reset/policy's gauge snapshot.
    pub fn sync(&mut self, data: &Data, configured_clients: impl Iterator<Item = u64>) {
        for (index, job) in data.jobs.iter().enumerate() {
            let previous = self.seen.get(index).copied();
            if previous.is_none() {
                let tags = self.tags(job, false);
                self.cpu.record(u64::from(job.cpu), &tags);
                self.memory.record(u64::from(job.memory_gib) * GIB, &tags);
                self.seen.push(job.phase);
            }
            if previous == Some(job.phase) {
                continue;
            }
            self.seen[index] = job.phase;
            let mut tags = self.tags(job, true);
            match job.phase {
                JobPhase::Running => self.wait.record(
                    (job.started_at.unwrap() - job.arrived_at) as f64 / 1000.0,
                    &tags,
                ),
                JobPhase::Completed | JobPhase::Rejected => {
                    let rejected = job.phase == JobPhase::Rejected;
                    tags.extend([
                        KeyValue::new("error", rejected),
                        KeyValue::new("outcome", if rejected { "rejected" } else { "completed" }),
                    ]);
                    if rejected {
                        let can_fit = data
                            .nodes
                            .iter()
                            .any(|n| job.cpu <= n.cpu && job.memory_gib <= n.memory_gib);
                        let reason = if can_fit { "queue_full" } else { "too_large" };
                        tags.push(KeyValue::new("reason", reason));
                        tracing::info!(target: "reflex_sim::scheduler", job_id = job.id, client = client(job.client), reason, "Job rejected");
                    }
                    self.jobs.add(1, &tags);
                    let end = job.completed_at.unwrap_or(data.at_ms);
                    self.duration
                        .record((end - job.arrived_at) as f64 / 1000.0, &tags);
                    if !rejected {
                        self.run_duration
                            .record((end - job.started_at.unwrap()) as f64 / 1000.0, &tags);
                    }
                }
                JobPhase::Queued => {}
            }
        }
        self.known_clients.extend(configured_clients);
        let clients: BTreeSet<_> = self
            .known_clients
            .iter()
            .copied()
            .chain(
                data.jobs
                    .iter()
                    .filter(|j| j.phase == JobPhase::Queued)
                    .map(|j| j.client),
            )
            .collect();
        self.known_clients.extend(clients.iter().copied());
        let queues = clients
            .into_iter()
            .map(|id| {
                let queued: Vec<_> = data
                    .jobs
                    .iter()
                    .filter(|j| j.client == id && j.phase == JobPhase::Queued)
                    .collect();
                let age = queued
                    .iter()
                    .map(|j| data.at_ms - j.arrived_at)
                    .max()
                    .unwrap_or(0);
                (
                    self.scoped(vec![
                        KeyValue::new("policy", self.policy),
                        KeyValue::new("client", client(id)),
                    ]),
                    queued.len() as u64,
                    age as f64 / 1000.0,
                )
            })
            .collect();
        let nodes = data
            .nodes
            .iter()
            .enumerate()
            .map(|(i, n)| {
                let running = data
                    .jobs
                    .iter()
                    .filter(|j| j.phase == JobPhase::Running && j.node == Some(i))
                    .count();
                (
                    self.scoped(vec![
                        KeyValue::new("policy", self.policy),
                        KeyValue::new("node", node(i)),
                    ]),
                    [
                        running as u64,
                        u64::from(n.cpu),
                        u64::from(n.used_cpu),
                        u64::from(n.memory_gib) * GIB,
                        u64::from(n.used_memory_gib) * GIB,
                    ],
                )
            })
            .collect();
        *self.snapshot.lock().unwrap_or_else(|e| e.into_inner()) = Snapshot { queues, nodes };
    }
    pub fn placement(&self, client_id: u64, selected_node: Option<usize>, outcome: &str) {
        let mut tags = vec![
            KeyValue::new("policy", self.policy),
            KeyValue::new("client", client(client_id)),
            KeyValue::new("outcome", outcome.to_owned()),
            KeyValue::new("error", matches!(outcome, "rejected" | "evaluation_error")),
        ];
        if let Some(index) = selected_node {
            tags.push(KeyValue::new("node", node(index)));
        }
        self.placements.add(1, &self.scoped(tags));
    }
}
fn duration(meter: &Meter, name: &'static str) -> Histogram<f64> {
    meter
        .f64_histogram(name)
        .with_unit("s")
        .with_description("Duration in simulated seconds")
        .with_boundaries(vec![
            0.0, 0.001, 0.01, 0.1, 0.5, 1.0, 2.0, 5.0, 10.0, 20.0, 30.0, 60.0, 120.0, 180.0,
        ])
        .build()
}
