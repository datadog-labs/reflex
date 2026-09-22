//! Records only committed simulation events; callbacks observe snapshots without locking the engine.
use super::{
    engine::{Data, Lifecycle, Request},
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

#[derive(Clone, Debug)]
pub(super) enum MetricEvent {
    Client {
        request: Request,
        success: bool,
        rejected: bool,
        reason: &'static str,
    },
    Attempt {
        request: Request,
        outcome: &'static str,
    },
    Wait(Request),
    Retry {
        request: Request,
        suppression: Option<&'static str>,
    },
}
pub(super) fn policy(value: Policy) -> &'static str {
    match value {
        Policy::Jev => "jev",
        Policy::Fixed => "fixed",
    }
}
pub(super) fn replica(id: usize) -> &'static str {
    ["replica_a", "replica_b", "replica_c", "replica_d"][id]
}
fn client(id: u64) -> String {
    format!("client_{}", id + 1)
}
type Sample = (&'static str, Vec<KeyValue>, f64);
pub(super) struct Telemetry {
    policy: &'static str,
    run: Option<String>,
    known_clients: BTreeSet<u64>,
    snapshot: Arc<Mutex<Vec<Sample>>>,
    last_rebuild: Option<(u64, u64, bool)>,
    client_requests: Counter<u64>,
    server_requests: Counter<u64>,
    retries: Counter<u64>,
    rebuilds: Counter<u64>,
    bytes: Counter<u64>,
    client_duration: Histogram<f64>,
    server_duration: Histogram<f64>,
    wait: Histogram<f64>,
    rebuild_duration: Histogram<f64>,
}
impl Telemetry {
    pub fn new(meter: Meter, selection: Policy, run: Option<String>) -> Self {
        let snapshot = Arc::new(Mutex::new(Vec::<Sample>::new()));
        for (name, unit) in [
            ("http.client.in_flight", "{request}"),
            ("recovery.replica.requests.outstanding", "{request}"),
            ("recovery.replica.heartbeat.age", "s"),
            ("http.server.queue.depth", "{request}"),
            ("http.server.active", "{request}"),
            ("recovery.retry.budget.available", "{credit}"),
            ("recovery.retry.enabled", "1"),
            ("recovery.replica.state", "1"),
            ("recovery.replica.serving", "1"),
            ("recovery.replica.reachable", "1"),
            ("recovery.serving.essential_only", "1"),
            ("recovery.intervention.required", "1"),
            ("recovery.rebuild.active", "1"),
        ] {
            let weak = Arc::downgrade(&snapshot);
            meter
                .f64_observable_gauge(name)
                .with_unit(unit)
                .with_callback(move |o| {
                    if let Some(snapshot) = weak.upgrade() {
                        for (metric, tags, value) in
                            snapshot.lock().unwrap_or_else(|e| e.into_inner()).iter()
                        {
                            if *metric == name {
                                o.observe(*value, tags);
                            }
                        }
                    }
                })
                .build();
        }
        Self {
            policy: policy(selection),
            run,
            known_clients: BTreeSet::new(),
            snapshot,
            last_rebuild: None,
            client_requests: meter
                .u64_counter("http.client.requests")
                .with_unit("{request}")
                .build(),
            server_requests: meter
                .u64_counter("http.server.requests")
                .with_unit("{request}")
                .build(),
            retries: meter
                .u64_counter("recovery.retries")
                .with_unit("{decision}")
                .build(),
            rebuilds: meter
                .u64_counter("recovery.rebuilds")
                .with_unit("{rebuild}")
                .build(),
            bytes: meter
                .u64_counter("recovery.rebuild.bytes")
                .with_unit("By")
                .build(),
            client_duration: duration(&meter, "http.client.request.duration"),
            server_duration: duration(&meter, "http.server.request.duration"),
            wait: duration(&meter, "http.server.queue.wait"),
            rebuild_duration: duration(&meter, "recovery.rebuild.duration"),
        }
    }
    fn base(&self) -> Vec<KeyValue> {
        let mut tags = vec![
            KeyValue::new("policy", self.policy),
            KeyValue::new("upstream", "replica_pool"),
        ];
        if let Some(run) = &self.run {
            tags.push(KeyValue::new("simulation_run", run.clone()));
        }
        tags
    }
    fn request(&self, r: &Request, server: bool) -> Vec<KeyValue> {
        let mut tags = self.base();
        tags.extend([
            KeyValue::new("client", client(r.client)),
            KeyValue::new("essential", r.essential),
        ]);
        if server {
            tags.extend([
                KeyValue::new("service", "replica_pool"),
                KeyValue::new("replica", replica(r.node)),
                KeyValue::new("retry", r.attempt > 0),
            ]);
        } else {
            tags.push(KeyValue::new("retried", r.attempt > 0));
        }
        tags
    }
    // Called exactly once following each successful state mutation, never on reads/rejected guards.
    pub fn sync(&mut self, d: &Data) {
        for event in &d.metric_events {
            match event {
                MetricEvent::Client {
                    request: r,
                    success,
                    rejected,
                    reason,
                } => {
                    let mut tags = self.request(r, false);
                    if !matches!(*reason, "no_serving_replica" | "essential_only") {
                        tags.push(KeyValue::new("replica", replica(r.node)));
                    }
                    tags.extend([
                        KeyValue::new("error", !success),
                        KeyValue::new(
                            "outcome",
                            if *success {
                                "success"
                            } else if *rejected {
                                "rejected"
                            } else {
                                "failure"
                            },
                        ),
                    ]);
                    if !success {
                        tags.push(KeyValue::new("reason", *reason));
                    }
                    let status = match *reason {
                        "success" => Some(200),
                        "server_error" => Some(500),
                        "queue_full" if r.received_at.is_some() => Some(503),
                        _ => None,
                    };
                    if let Some(status) = status {
                        tags.push(KeyValue::new("http.status_code", status));
                    }
                    self.client_requests.add(1, &tags);
                    self.client_duration
                        .record((d.at_ms - r.arrived_at) as f64 / 1000., &tags);
                }
                MetricEvent::Attempt {
                    request: r,
                    outcome,
                } => {
                    // A partition/crash can prevent a request from ever reaching the replica.
                    let Some(received) = r.received_at else {
                        continue;
                    };
                    let mut tags = self.request(r, true);
                    tags.extend([
                        KeyValue::new("error", *outcome != "success"),
                        KeyValue::new("outcome", *outcome),
                    ]);
                    let status = match *outcome {
                        "success" => Some(200),
                        "server_error" => Some(500),
                        "queue_full" => Some(503),
                        _ => None,
                    };
                    if let Some(status) = status {
                        tags.push(KeyValue::new("http.status_code", status));
                    }
                    self.server_requests.add(1, &tags);
                    self.server_duration
                        .record((d.at_ms - received) as f64 / 1000., &tags);
                    if r.started_at.is_none() {
                        self.wait
                            .record((d.at_ms - received) as f64 / 1000., &self.request(r, true));
                    }
                }
                MetricEvent::Wait(r) => {
                    self.wait.record(
                        (r.started_at.unwrap() - r.received_at.unwrap()) as f64 / 1000.,
                        &self.request(r, true),
                    );
                }
                MetricEvent::Retry {
                    request: r,
                    suppression,
                } => {
                    let mut tags = self.request(r, false);
                    tags.push(KeyValue::new(
                        "outcome",
                        if suppression.is_some() {
                            "suppressed"
                        } else {
                            "attempted"
                        },
                    ));
                    if let Some(reason) = suppression {
                        tags.push(KeyValue::new("reason", *reason));
                    }
                    self.retries.add(1, &tags);
                }
            }
        }
        if let Some(r) = &d.recovery {
            let tags = self.rebuild_tags(r.source, r.target);
            let bytes = (r.progress_mb * 1_000_000.).round() as u64;
            let previous = self.last_rebuild.filter(|(id, _, _)| *id == r.id);
            let delta = bytes.saturating_sub(previous.map_or(0, |(_, b, _)| b));
            if delta > 0 {
                self.bytes.add(delta, &tags);
            }
            if !r.active() && !previous.is_some_and(|(_, _, done)| done) {
                let mut tags = tags;
                tags.push(KeyValue::new("outcome", r.phase.clone()));
                self.rebuilds.add(1, &tags);
                self.rebuild_duration.record(
                    (r.finished_at.unwrap() - r.started_at) as f64 / 1000.,
                    &tags,
                );
                tracing::info!(target: "reflex_sim::recovery", rebuild_id = r.id, source = replica(r.source), target = replica(r.target), outcome = r.phase, reason = r.reason, "Rebuild finished");
            }
            self.last_rebuild = Some((r.id, bytes, !r.active()));
        }
        let mut samples = vec![];
        let mut add = |name, tags, value| samples.push((name, tags, value));
        let clients: BTreeSet<_> = d
            .clients
            .iter()
            .map(|c| c.id)
            .chain(d.requests.iter().map(|r| r.client))
            .collect();
        self.known_clients.extend(clients);
        for id in self.known_clients.iter().copied() {
            let mut tags = self.base();
            tags.push(KeyValue::new("client", client(id)));
            add(
                "http.client.in_flight",
                tags,
                d.requests.iter().filter(|r| r.client == id).count() as f64,
            );
        }
        for n in &d.replicas {
            let mut tags = self.base();
            tags.push(KeyValue::new("replica", replica(n.id)));
            add(
                "recovery.replica.requests.outstanding",
                tags.clone(),
                d.requests.iter().filter(|r| r.node == n.id).count() as f64,
            );
            add(
                "recovery.replica.heartbeat.age",
                tags.clone(),
                (d.at_ms - n.heartbeat_at) as f64 / 1000.,
            );
            for (state, label) in [
                (Lifecycle::Empty, "empty"),
                (Lifecycle::Rebuilding, "rebuilding"),
                (Lifecycle::Checking, "checking"),
                (Lifecycle::Ready, "ready"),
                (Lifecycle::Unavailable, "unavailable"),
            ] {
                let mut t = tags.clone();
                t.push(KeyValue::new("state", label));
                add(
                    "recovery.replica.state",
                    t,
                    u8::from(n.phase == state) as f64,
                );
            }
            add(
                "recovery.replica.serving",
                tags.clone(),
                u8::from(n.serving) as f64,
            );
            for (path, value) in [("client", n.reachable), ("transfer", n.transfer_reachable)] {
                let mut t = tags.clone();
                t.push(KeyValue::new("path", path));
                add("recovery.replica.reachable", t, u8::from(value) as f64);
            }
            tags.push(KeyValue::new("service", "replica_pool"));
            let received: Vec<_> = d
                .requests
                .iter()
                .filter(|r| r.node == n.id && r.received_at.is_some())
                .collect();
            add(
                "http.server.queue.depth",
                tags.clone(),
                received.iter().filter(|r| r.started_at.is_none()).count() as f64,
            );
            add(
                "http.server.active",
                tags,
                received.iter().filter(|r| r.started_at.is_some()).count() as f64,
            );
        }
        for (name, value) in [
            ("recovery.retry.budget.available", d.retry_credit),
            ("recovery.retry.enabled", u8::from(d.retries_enabled) as f64),
            (
                "recovery.serving.essential_only",
                u8::from(d.essential_only) as f64,
            ),
            (
                "recovery.intervention.required",
                u8::from(d.intervention) as f64,
            ),
        ] {
            add(name, self.base(), value);
        }
        // Emit zeros for inactive pairs/phases so a prior rebuild cannot look active after replacement.
        for source in 0..d.replicas.len() {
            for target in 0..d.replicas.len() {
                if source == target {
                    continue;
                }
                for phase in ["rebuilding", "verifying"] {
                    let mut tags = self.rebuild_tags(source, target);
                    tags.push(KeyValue::new("phase", phase));
                    let active = d.recovery.as_ref().is_some_and(|r| {
                        r.source == source && r.target == target && r.phase == phase
                    });
                    add("recovery.rebuild.active", tags, u8::from(active) as f64);
                }
            }
        }
        *self.snapshot.lock().unwrap_or_else(|e| e.into_inner()) = samples;
    }
    fn rebuild_tags(&self, source: usize, target: usize) -> Vec<KeyValue> {
        let mut tags = self.base();
        tags.extend([
            KeyValue::new("source", replica(source)),
            KeyValue::new("target", replica(target)),
        ]);
        tags
    }
}
fn duration(meter: &Meter, name: &'static str) -> Histogram<f64> {
    meter
        .f64_histogram(name)
        .with_unit("s")
        .with_description("Duration in simulated seconds")
        .with_boundaries(vec![
            0., 0.001, 0.01, 0.05, 0.1, 0.25, 0.5, 1., 1.5, 2., 3., 5., 10., 30., 60., 180.,
        ])
        .build()
}
