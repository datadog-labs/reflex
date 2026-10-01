// Unless explicitly stated otherwise all files in this repository are licensed under the Apache-2.0 License.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-present Datadog, Inc.

//! Cluster autoscaler metrics. No exporter or subscriber is installed here.
use super::engine::{Choice, Data, Group, NodePhase, Outcome, Phase};
use opentelemetry::{
    metrics::{Counter, Histogram, Meter},
    KeyValue,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex},
};

const GIB: u64 = 1 << 30;
pub(super) const STATES: [(NodePhase, &str); 3] = [
    (NodePhase::Provisioning, "provisioning"),
    (NodePhase::Ready, "ready"),
    (NodePhase::Draining, "draining"),
];
pub(super) fn phase(phase: Phase) -> &'static str {
    match phase {
        Phase::Stable => "stable",
        Phase::ScalingUp => "scaling_up",
        Phase::ScalingDown => "scaling_down",
    }
}
#[derive(Default)]
struct Snapshot {
    /// Requested CPU, requested memory, ready CPU and ready memory; absent before the first sync.
    cluster: Option<(Vec<KeyValue>, [u64; 4])>,
    /// Pending, desired and available replicas, and the oldest pending age, per workload.
    workloads: Vec<(Vec<KeyValue>, [u64; 3], f64)>,
    /// CPU capacity and reservation, memory capacity and reservation, and cost rate, per group.
    groups: Vec<(Vec<KeyValue>, [u64; 4], f64)>,
    nodes: Vec<(Vec<KeyValue>, u64)>,
    phases: Vec<(Vec<KeyValue>, u64)>,
}
type Shared = Arc<Mutex<Snapshot>>;
fn observe_u64(
    meter: &Meter,
    snapshot: &Shared,
    name: &'static str,
    unit: &'static str,
    each: impl Fn(&Snapshot, &mut dyn FnMut(u64, &[KeyValue])) + Send + Sync + 'static,
) {
    let weak = Arc::downgrade(snapshot);
    meter
        .u64_observable_gauge(name)
        .with_unit(unit)
        .with_callback(move |observer| {
            if let Some(s) = weak.upgrade() {
                each(
                    &s.lock().unwrap_or_else(|e| e.into_inner()),
                    &mut |value, tags| observer.observe(value, tags),
                );
            }
        })
        .build();
}
fn observe_f64(
    meter: &Meter,
    snapshot: &Shared,
    name: &'static str,
    unit: &'static str,
    each: impl Fn(&Snapshot, &mut dyn FnMut(f64, &[KeyValue])) + Send + Sync + 'static,
) {
    let weak = Arc::downgrade(snapshot);
    meter
        .f64_observable_gauge(name)
        .with_unit(unit)
        .with_callback(move |observer| {
            if let Some(s) = weak.upgrade() {
                each(
                    &s.lock().unwrap_or_else(|e| e.into_inner()),
                    &mut |value, tags| observer.observe(value, tags),
                );
            }
        })
        .build();
}
fn tags<const N: usize>(base: &[KeyValue], extra: [KeyValue; N]) -> Vec<KeyValue> {
    base.iter().cloned().chain(extra).collect()
}
pub(super) struct Telemetry {
    base: Vec<KeyValue>,
    snapshot: Shared,
    decisions: Counter<u64>,
    node_events: Counter<u64>,
    pending_time: Counter<f64>,
    pending_wait: Histogram<f64>,
    provision: Histogram<f64>,
    primed: bool,
    nodes: BTreeMap<u64, NodePhase>,
    pods: BTreeSet<u64>,
    waiting: BTreeMap<u64, u64>,
    pending_pod_ms: u64,
}
impl Telemetry {
    /// Every metric carries `policy:jev` and this run's `simulation_run`.
    pub fn new(meter: &Meter, run: &str) -> Self {
        let snapshot = Shared::default();
        for (index, name, unit) in [
            (0, "autoscaler.cpu.requested", "{cpu}"),
            (1, "autoscaler.memory.requested", "By"),
            (2, "autoscaler.cpu.ready", "{cpu}"),
            (3, "autoscaler.memory.ready", "By"),
        ] {
            observe_u64(meter, &snapshot, name, unit, move |s, observe| {
                if let Some((tags, values)) = &s.cluster {
                    observe(values[index], tags);
                }
            });
        }
        for (index, name) in [
            (0, "autoscaler.pods.pending"),
            (1, "autoscaler.workload.replicas.desired"),
            (2, "autoscaler.workload.replicas.available"),
        ] {
            observe_u64(meter, &snapshot, name, "{pod}", move |s, observe| {
                for (tags, values, _) in &s.workloads {
                    observe(values[index], tags);
                }
            });
        }
        observe_f64(
            meter,
            &snapshot,
            "autoscaler.pods.pending.oldest_age",
            "s",
            |s, observe| {
                for (tags, _, age) in &s.workloads {
                    observe(*age, tags);
                }
            },
        );
        for (index, name, unit) in [
            (0, "autoscaler.group.cpu.capacity", "{cpu}"),
            (1, "autoscaler.group.cpu.reserved", "{cpu}"),
            (2, "autoscaler.group.memory.capacity", "By"),
            (3, "autoscaler.group.memory.reserved", "By"),
        ] {
            observe_u64(meter, &snapshot, name, unit, move |s, observe| {
                for (tags, values, _) in &s.groups {
                    observe(values[index], tags);
                }
            });
        }
        observe_f64(
            meter,
            &snapshot,
            "autoscaler.cost.rate",
            "{USD}/h",
            |s, observe| {
                for (tags, _, rate) in &s.groups {
                    observe(*rate, tags);
                }
            },
        );
        observe_u64(
            meter,
            &snapshot,
            "autoscaler.nodes",
            "{node}",
            |s, observe| {
                for (tags, count) in &s.nodes {
                    observe(*count, tags);
                }
            },
        );
        observe_u64(meter, &snapshot, "autoscaler.phase", "1", |s, observe| {
            for (tags, active) in &s.phases {
                observe(*active, tags);
            }
        });
        let seconds = |name: &'static str, description: &'static str| {
            meter
                .f64_histogram(name)
                .with_unit("s")
                .with_description(description)
                .with_boundaries(vec![
                    0.0, 1.0, 2.0, 5.0, 10.0, 20.0, 30.0, 45.0, 60.0, 90.0, 120.0, 300.0,
                ])
                .build()
        };
        Self {
            base: vec![
                KeyValue::new("policy", "jev"),
                KeyValue::new("simulation_run", run.to_owned()),
            ],
            snapshot,
            decisions: meter
                .u64_counter("autoscaler.decisions")
                .with_unit("{decision}")
                .with_description("Jev recommendations by action and what Reflex did with them")
                .build(),
            node_events: meter
                .u64_counter("autoscaler.node.events")
                .with_unit("{node}")
                .with_description(
                    "Node lifecycle steps: requested, ready, failed, draining, removed",
                )
                .build(),
            pending_time: meter
                .f64_counter("autoscaler.pods.pending.time")
                .with_unit("s")
                .with_description("Pod-seconds spent pending, in simulated seconds")
                .build(),
            pending_wait: seconds(
                "autoscaler.pod.pending.duration",
                "Simulated seconds a pod waited before it was placed; zero when placed at once",
            ),
            provision: seconds(
                "autoscaler.node.provision.duration",
                "Simulated seconds from a node request to the node being ready",
            ),
            primed: false,
            nodes: BTreeMap::new(),
            pods: BTreeSet::new(),
            waiting: BTreeMap::new(),
            pending_pod_ms: 0,
        }
    }
    // Observe committed state after every clock stop, control and decision. A fresh instance
    // owns each run's gauge snapshot, so a reset never leaves the old run's values behind.
    pub fn sync(&mut self, current: Phase, d: &Data) {
        let base = self.base.clone();
        for n in &d.nodes {
            let previous = self.nodes.insert(n.id, n.phase);
            let event = match (previous, n.phase) {
                (None, NodePhase::Provisioning) => "requested",
                (Some(NodePhase::Provisioning), NodePhase::Ready) => "ready",
                (Some(NodePhase::Provisioning), NodePhase::Removed) => "failed",
                (Some(NodePhase::Ready), NodePhase::Draining) => "draining",
                (Some(NodePhase::Draining), NodePhase::Removed) => "removed",
                _ => continue,
            };
            let event_tags = tags(
                &base,
                [
                    KeyValue::new("group", n.group.key()),
                    KeyValue::new("event", event),
                ],
            );
            self.node_events.add(1, &event_tags);
            if let (true, Some(ready)) = (event == "ready", n.ready_at) {
                self.provision.record(
                    (ready - n.requested_at) as f64 / 1000.,
                    &tags(&base, [KeyValue::new("group", n.group.key())]),
                );
            }
        }
        for p in &d.pods {
            let workload = || {
                tags(
                    &base,
                    [KeyValue::new("workload", d.workloads[p.workload].name)],
                )
            };
            let known = !self.pods.insert(p.id);
            match (p.node, p.pending_since) {
                (None, since) => {
                    self.waiting.insert(p.id, since.unwrap_or(d.at_ms));
                }
                (Some(_), _) => {
                    if let Some(since) = self.waiting.remove(&p.id) {
                        self.pending_wait
                            .record((d.at_ms - since) as f64 / 1000., &workload());
                    } else if !known && self.primed {
                        self.pending_wait.record(0., &workload());
                    }
                }
            }
        }
        // A pod deleted while pending never waited to completion: forget it without a sample.
        self.waiting
            .retain(|id, _| d.pods.iter().any(|p| p.id == *id));
        self.primed = true;
        if d.pending_pod_ms > self.pending_pod_ms {
            self.pending_time.add(
                (d.pending_pod_ms - self.pending_pod_ms) as f64 / 1000.,
                &self.base,
            );
            self.pending_pod_ms = d.pending_pod_ms;
        }
        let (requested, ready) = (d.requested(), d.ready_capacity());
        let workloads = d
            .workloads
            .iter()
            .enumerate()
            .map(|(i, w)| {
                let pending = d
                    .pods
                    .iter()
                    .filter(|p| p.workload == i && p.node.is_none());
                let oldest = pending
                    .clone()
                    .filter_map(|p| p.pending_since)
                    .map(|t| d.at_ms - t)
                    .max()
                    .unwrap_or(0);
                (
                    tags(&base, [KeyValue::new("workload", w.name)]),
                    [
                        pending.count() as u64,
                        u64::from(w.desired()),
                        u64::from(d.available(i)),
                    ],
                    oldest as f64 / 1000.,
                )
            })
            .collect();
        let groups = d
            .groups
            .iter()
            .map(|g| {
                let ready = || {
                    d.nodes
                        .iter()
                        .filter(|n| n.group == g.group && n.phase == NodePhase::Ready)
                };
                let sum = |value: fn(&super::engine::Node) -> u32| -> u64 {
                    ready().map(|n| u64::from(value(n))).sum()
                };
                (
                    tags(&base, [KeyValue::new("group", g.group.key())]),
                    [
                        sum(|n| n.cpu),
                        sum(|n| n.used_cpu),
                        sum(|n| n.memory_gib) * GIB,
                        sum(|n| n.used_memory_gib) * GIB,
                    ],
                    d.active(g.group) as f64 * g.hourly_usd,
                )
            })
            .collect();
        // Every group and state is always reported, so an emptied state reads zero, not stale.
        let nodes = Group::ALL
            .iter()
            .flat_map(|group| {
                let base = &base;
                STATES.iter().map(move |(state, name)| {
                    (
                        tags(
                            base,
                            [
                                KeyValue::new("group", group.key()),
                                KeyValue::new("state", *name),
                            ],
                        ),
                        d.count(Some(*group), *state) as u64,
                    )
                })
            })
            .collect();
        let phases = [Phase::Stable, Phase::ScalingUp, Phase::ScalingDown]
            .into_iter()
            .map(|p| {
                (
                    tags(&base, [KeyValue::new("phase", phase(p))]),
                    u64::from(p == current),
                )
            })
            .collect();
        *self.snapshot.lock().unwrap_or_else(|e| e.into_inner()) = Snapshot {
            cluster: Some((
                self.base.clone(),
                [
                    u64::from(requested.cpu),
                    u64::from(requested.memory_gib) * GIB,
                    u64::from(ready.cpu),
                    u64::from(ready.memory_gib) * GIB,
                ],
            )),
            workloads,
            groups,
            nodes,
            phases,
        };
    }
    /// One count per recommendation Reflex processed, including evaluation errors.
    pub fn decision(&self, choice: Option<Choice>, outcome: &Outcome) {
        let (action, group) = match choice {
            Some(Choice::ScaleUp { group, .. }) => ("scale_up", Some(group)),
            Some(Choice::Remove { group, .. }) => ("remove", Some(group)),
            Some(Choice::NoChange) => ("no_change", None),
            None => ("none", None),
        };
        let mut tags = tags(
            &self.base,
            [
                KeyValue::new("action", action),
                KeyValue::new("outcome", outcome.status),
                KeyValue::new(
                    "error",
                    matches!(outcome.status, "rejected" | "evaluation_error"),
                ),
            ],
        );
        if let Some(group) = group {
            tags.push(KeyValue::new("group", group.key()));
        }
        if let (true, Some(code)) = (outcome.status == "rejected", &outcome.code) {
            tags.push(KeyValue::new("guard", code.clone()));
        }
        self.decisions.add(1, &tags);
    }
}
