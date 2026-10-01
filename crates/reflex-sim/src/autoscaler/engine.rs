// Unless explicitly stated otherwise all files in this repository are licensed under the Apache-2.0 License.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-present Datadog, Inc.

use crate::Error;
use reflex::{
    state_machine, ExecutionOutcome, InMemory, Judgment, Rejection, StateMachineExecutor,
};
use serde::{Deserialize, Serialize};
use std::time::Instant;

pub const HORIZON_MS: u64 = 600_000;
/// Nominal provisioning time. Each node draws its actual time from the seed, within
/// `PROVISION_JITTER_MS` either side; evidence only ever carries the nominal estimate.
pub const PROVISION_MS: u64 = 30_000;
pub const PROVISION_JITTER_MS: u64 = 5_000;
pub const STOCKOUT_FAILURE_MS: u64 = 10_000;
pub const DRAIN_MS: u64 = 10_000;
pub const POD_START_MS: u64 = 5_000;
pub const EVIDENCE_TTL_MS: u64 = 5_000;
pub const SCALE_DOWN_COOLDOWN_MS: u64 = 30_000;
pub const NODE_BUDGET: usize = 10;
pub const SURGE_FACTOR: u32 = 3;
pub const MEMORY_HEAVY_FACTOR: u32 = 4;
pub const MAX_REPLICAS: u32 = 12;
pub const WORKLOADS: [&str; 3] = ["web", "api", "batch"];
/// The most capacity a forecast alone can justify: three large general nodes.
pub const HEADROOM_CAP: Resources = Resources {
    cpu: 24,
    memory_gib: 48,
};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Resources {
    pub cpu: u32,
    pub memory_gib: u32,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Stable,
    ScalingUp,
    ScalingDown,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Group {
    GeneralSmall,
    GeneralLarge,
    MemoryHeavy,
}
impl Group {
    pub const ALL: [Self; 3] = [Self::GeneralSmall, Self::GeneralLarge, Self::MemoryHeavy];
    pub fn index(self) -> usize {
        self as usize
    }
    pub fn key(self) -> &'static str {
        ["general_small", "general_large", "memory_heavy"][self.index()]
    }
    pub fn prefix(self) -> &'static str {
        ["small", "large", "mem"][self.index()]
    }
}
#[derive(Debug, Clone, Serialize)]
pub struct NodeGroup {
    pub group: Group,
    pub name: &'static str,
    pub cpu: u32,
    pub memory_gib: u32,
    pub min: usize,
    pub max: usize,
    pub hourly_usd: f64,
    pub stockout: bool,
    pub failures: u32,
    pub last_failure_at: Option<u64>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodePhase {
    Provisioning,
    Ready,
    Draining,
    Removed,
}
#[derive(Debug, Clone, Serialize)]
pub struct Node {
    pub id: u64,
    pub name: String,
    pub group: Group,
    pub phase: NodePhase,
    pub cpu: u32,
    pub memory_gib: u32,
    pub used_cpu: u32,
    pub used_memory_gib: u32,
    pub requested_at: u64,
    pub ready_at: Option<u64>,
    pub removed_at: Option<u64>,
    pub failed: bool,
    #[serde(skip_serializing)]
    pub operation: u64,
    /// When provisioning or draining completes. Drawn from the seed; never shown to Jev.
    #[serde(skip_serializing)]
    pub due_at: Option<u64>,
    #[serde(skip_serializing)]
    pub doomed: bool,
}
impl Node {
    pub fn free(&self) -> Resources {
        Resources {
            cpu: self.cpu - self.used_cpu,
            memory_gib: self.memory_gib - self.used_memory_gib,
        }
    }
    fn fits(&self, cpu: u32, memory_gib: u32) -> bool {
        let free = self.free();
        self.phase == NodePhase::Ready && cpu <= free.cpu && memory_gib <= free.memory_gib
    }
}
#[derive(Debug, Clone, Serialize)]
pub struct Workload {
    pub name: &'static str,
    pub cpu: u32,
    pub memory_gib: u32,
    /// Replicas wanted by current load, before any surge.
    pub replicas: u32,
    pub surge: bool,
    pub memory_heavy: bool,
}
impl Workload {
    pub fn desired(&self) -> u32 {
        self.replicas * if self.surge { SURGE_FACTOR } else { 1 }
    }
    pub fn pod_memory_gib(&self) -> u32 {
        self.memory_gib
            * if self.memory_heavy {
                MEMORY_HEAVY_FACTOR
            } else {
                1
            }
    }
    /// Disruption budget: at least half of the desired replicas stay available.
    pub fn min_available(&self) -> u32 {
        self.desired() / 2
    }
}
#[derive(Debug, Clone, Serialize)]
pub struct Pod {
    pub id: u64,
    pub workload: usize,
    pub cpu: u32,
    pub memory_gib: u32,
    pub node: Option<u64>,
    pub created_at: u64,
    pub pending_since: Option<u64>,
    /// A placed pod serves once it has started.
    pub available_at: Option<u64>,
}
#[derive(Debug, Clone, Serialize)]
pub struct Data {
    pub at_ms: u64,
    pub revision: u64,
    pub seed: u64,
    pub node_budget: usize,
    pub workloads: Vec<Workload>,
    pub groups: Vec<NodeGroup>,
    pub nodes: Vec<Node>,
    pub pods: Vec<Pod>,
    pub last_change_at: Option<u64>,
    pub last_scale_up_finished_at: Option<u64>,
    /// Billed node time per group and accumulated pending time, as exact integers.
    pub node_ms: [u64; 3],
    pub pending_pod_ms: u64,
    #[serde(skip_serializing)]
    pub operation: u64,
    #[serde(skip_serializing)]
    next_node: u64,
    #[serde(skip_serializing)]
    next_pod: u64,
}
impl Data {
    pub fn group(&self, group: Group) -> &NodeGroup {
        &self.groups[group.index()]
    }
    pub fn node(&self, id: u64) -> Option<&Node> {
        self.nodes.iter().find(|n| n.id == id)
    }
    pub fn count(&self, group: Option<Group>, phase: NodePhase) -> usize {
        self.nodes
            .iter()
            .filter(|n| n.phase == phase && group.is_none_or(|g| g == n.group))
            .count()
    }
    /// Nodes that exist in a group, including any still draining.
    pub fn active(&self, group: Group) -> usize {
        self.nodes
            .iter()
            .filter(|n| n.group == group && n.phase != NodePhase::Removed)
            .count()
    }
    pub fn active_nodes(&self) -> usize {
        Group::ALL.iter().map(|g| self.active(*g)).sum()
    }
    /// Nodes that are or will be serving: ready plus provisioning.
    pub fn serving(&self, group: Group) -> usize {
        self.count(Some(group), NodePhase::Ready) + self.count(Some(group), NodePhase::Provisioning)
    }
    pub fn pending(&self) -> Vec<&Pod> {
        let mut pods: Vec<_> = self.pods.iter().filter(|p| p.node.is_none()).collect();
        pods.sort_by_key(|p| (p.pending_since, p.id));
        pods
    }
    pub fn pending_count(&self) -> usize {
        self.pods.iter().filter(|p| p.node.is_none()).count()
    }
    pub fn oldest_pending_ms(&self) -> u64 {
        self.pods
            .iter()
            .filter_map(|p| p.pending_since)
            .map(|t| self.at_ms - t)
            .max()
            .unwrap_or(0)
    }
    pub fn available(&self, workload: usize) -> u32 {
        self.pods
            .iter()
            .filter(|p| p.workload == workload && self.is_available(p))
            .count() as u32
    }
    fn is_available(&self, pod: &Pod) -> bool {
        pod.node.is_some() && pod.available_at.is_some_and(|t| t <= self.at_ms)
    }
    /// CPU and memory requested by every pod, placed or pending.
    pub fn requested(&self) -> Resources {
        Resources {
            cpu: self.pods.iter().map(|p| p.cpu).sum(),
            memory_gib: self.pods.iter().map(|p| p.memory_gib).sum(),
        }
    }
    pub fn ready_capacity(&self) -> Resources {
        let ready = || self.nodes.iter().filter(|n| n.phase == NodePhase::Ready);
        Resources {
            cpu: ready().map(|n| n.cpu).sum(),
            memory_gib: ready().map(|n| n.memory_gib).sum(),
        }
    }
    pub fn hourly_usd(&self) -> f64 {
        Group::ALL
            .iter()
            .map(|g| self.active(*g) as f64 * self.group(*g).hourly_usd)
            .sum()
    }
    pub fn cost_usd(&self) -> f64 {
        Group::ALL
            .iter()
            .map(|g| self.node_ms[g.index()] as f64 * self.group(*g).hourly_usd / 3_600_000.)
            .sum()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Choice {
    NoChange,
    ScaleUp { group: Group, count: u8 },
    Remove { group: Group, node: u64 },
}
impl Choice {
    /// Stable wire label: the TypeSafe option, the evidence entry and the record all use it.
    pub fn label(&self) -> String {
        match self {
            Self::NoChange => "no_change".into(),
            Self::ScaleUp { group, count } => format!("scale_up:{}:{count}", group.key()),
            Self::Remove { group, node } => format!("remove:{}", node_name(*group, *node)),
        }
    }
}
impl Serialize for Choice {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.label())
    }
}
pub fn node_name(group: Group, id: u64) -> String {
    format!("{}-{id}", group.prefix())
}
/// A recommendation, tied to the cluster revision and time it was based on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Action {
    pub choice: Choice,
    pub revision: u64,
    pub observed_at: u64,
    /// Extra demand a fresh forecast expects. Justifies provisioning only; never usable capacity.
    pub headroom: Resources,
    /// Simulated time after which that forecast is too old to justify anything.
    pub headroom_expires_at: u64,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Control {
    ReplicaSurge { workload: usize, enabled: bool },
    MemoryHeavy { workload: usize, enabled: bool },
    Stockout { group: Group, enabled: bool },
}
#[derive(Debug, Clone)]
pub enum Event {
    Clock(u64),
    Demand { workload: usize, replicas: u32 },
    Control(Control),
    NodeReady { node: u64, last: bool },
    ProvisioningFailed { node: u64, last: bool },
    NodeDrained { node: u64 },
}
#[derive(Debug, Clone, Serialize)]
pub struct Transition {
    pub at_ms: u64,
    pub from: Phase,
    pub to: Phase,
    pub trigger: String,
}
#[derive(Debug, Clone, Serialize)]
pub struct Outcome {
    pub status: &'static str,
    pub code: Option<String>,
    pub reason: String,
    pub from: Phase,
    pub to: Phase,
}

pub(super) fn draw(seed: u64, stream: u64, n: u64) -> u64 {
    let mut bits = seed ^ stream.wrapping_mul(0x9e3779b97f4a7c15) ^ n;
    bits = (bits ^ (bits >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    bits = (bits ^ (bits >> 27)).wrapping_mul(0x94d049bb133111eb);
    bits ^ (bits >> 31)
}
/// Deterministic best fit: the ready node left with the least spare share after placement.
fn best_fit(nodes: &[Node], cpu: u32, memory_gib: u32) -> Option<usize> {
    let spare = |n: &Node| {
        let free = n.free();
        (free.cpu - cpu) as f64 / n.cpu as f64
            + (free.memory_gib - memory_gib) as f64 / n.memory_gib as f64
    };
    nodes
        .iter()
        .enumerate()
        .filter(|(_, n)| n.fits(cpu, memory_gib))
        .min_by(|(_, a), (_, b)| spare(a).total_cmp(&spare(b)).then(a.id.cmp(&b.id)))
        .map(|(i, _)| i)
}
fn place(d: &mut Data, pod: usize, node: usize) {
    let (cpu, memory_gib) = (d.pods[pod].cpu, d.pods[pod].memory_gib);
    d.nodes[node].used_cpu += cpu;
    d.nodes[node].used_memory_gib += memory_gib;
    d.pods[pod].node = Some(d.nodes[node].id);
    d.pods[pod].pending_since = None;
    d.pods[pod].available_at = Some(d.at_ms + POD_START_MS);
}
fn release(d: &mut Data, pod: usize) {
    if let Some(id) = d.pods[pod].node.take() {
        let (cpu, memory_gib) = (d.pods[pod].cpu, d.pods[pod].memory_gib);
        let node = d.nodes.iter_mut().find(|n| n.id == id).expect("placed pod");
        node.used_cpu -= cpu;
        node.used_memory_gib -= memory_gib;
    }
}
/// Place waiting pods, longest wait first. A pod that fits no ready node stays pending.
fn schedule(d: &mut Data) {
    let mut waiting: Vec<_> = (0..d.pods.len())
        .filter(|i| d.pods[*i].node.is_none())
        .collect();
    waiting.sort_by_key(|i| (d.pods[*i].pending_since, d.pods[*i].id));
    for pod in waiting {
        if let Some(node) = best_fit(&d.nodes, d.pods[pod].cpu, d.pods[pod].memory_gib) {
            place(d, pod, node);
        }
    }
}
/// Create or delete pods until a workload has its desired replicas.
fn reconcile(d: &mut Data, workload: usize) {
    let w = &d.workloads[workload];
    let (desired, cpu, memory_gib) = (w.desired() as usize, w.cpu, w.pod_memory_gib());
    let mut current = d.pods.iter().filter(|p| p.workload == workload).count();
    while current < desired {
        d.next_pod += 1;
        d.pods.push(Pod {
            id: d.next_pod,
            workload,
            cpu,
            memory_gib,
            node: None,
            created_at: d.at_ms,
            pending_since: Some(d.at_ms),
            available_at: None,
        });
        current += 1;
    }
    while current > desired {
        // Pending replicas go first. Otherwise the seed picks one, so scaling in leaves
        // partly used nodes rather than conveniently empty ones.
        let victim = (0..d.pods.len())
            .filter(|i| d.pods[*i].workload == workload)
            .max_by_key(|i| {
                let p = &d.pods[*i];
                (p.node.is_none(), draw(d.seed, p.id, d.revision), p.id)
            })
            .expect("a replica to delete");
        release(d, victim);
        d.pods.remove(victim);
        current -= 1;
    }
}
/// Where each pod on a node would go if it were drained now; `None` when one fits nowhere.
fn plan_drain(d: &Data, node: u64) -> Option<Vec<(usize, u64)>> {
    let mut others: Vec<Node> = d.nodes.iter().filter(|n| n.id != node).cloned().collect();
    let mut evicted: Vec<_> = (0..d.pods.len())
        .filter(|i| d.pods[*i].node == Some(node))
        .collect();
    evicted.sort_by_key(|i| {
        let p = &d.pods[*i];
        (std::cmp::Reverse((p.memory_gib, p.cpu)), p.id)
    });
    let mut moves = Vec::new();
    for pod in evicted {
        let p = &d.pods[pod];
        let target = best_fit(&others, p.cpu, p.memory_gib)?;
        others[target].used_cpu += p.cpu;
        others[target].used_memory_gib += p.memory_gib;
        moves.push((pod, others[target].id));
    }
    Some(moves)
}
/// Pending pods that would still fit nowhere once every provisioning node is ready, and the
/// spare capacity left for new pods after placing the rest. Fragments too small for any
/// current pod shape are not counted as spare.
pub fn coverage(d: &Data) -> (usize, Resources) {
    let mut nodes: Vec<Node> = d
        .nodes
        .iter()
        .filter(|n| matches!(n.phase, NodePhase::Ready | NodePhase::Provisioning))
        .cloned()
        .collect();
    for n in &mut nodes {
        n.phase = NodePhase::Ready;
    }
    let mut uncovered = 0;
    for p in d.pending() {
        match best_fit(&nodes, p.cpu, p.memory_gib) {
            Some(i) => {
                nodes[i].used_cpu += p.cpu;
                nodes[i].used_memory_gib += p.memory_gib;
            }
            None => uncovered += 1,
        }
    }
    let mut spare = Resources::default();
    for n in &nodes {
        if d.workloads
            .iter()
            .any(|w| n.fits(w.cpu, w.pod_memory_gib()))
        {
            spare.cpu += n.free().cpu;
            spare.memory_gib += n.free().memory_gib;
        }
    }
    (uncovered, spare)
}

pub fn invariant(phase: &Phase, d: &Data) -> Result<(), Rejection> {
    for n in &d.nodes {
        let pods = || d.pods.iter().filter(|p| p.node == Some(n.id));
        let cpu: u32 = pods().map(|p| p.cpu).sum();
        let memory_gib: u32 = pods().map(|p| p.memory_gib).sum();
        if n.used_cpu > n.cpu || n.used_memory_gib > n.memory_gib {
            return Err(Rejection::new(
                "overcommitted",
                "A node cannot reserve more CPU or memory than it has",
            ));
        }
        if cpu != n.used_cpu || memory_gib != n.used_memory_gib {
            return Err(Rejection::new(
                "reservations",
                "Node reservations must equal the pods placed on the node",
            ));
        }
    }
    if d.pods.iter().any(|p| {
        p.node
            .is_some_and(|id| d.node(id).is_none_or(|n| n.phase != NodePhase::Ready))
    }) {
        return Err(Rejection::new(
            "pod_assignment",
            "Pods may only be placed on ready nodes",
        ));
    }
    for g in &d.groups {
        if d.active(g.group) > g.max || d.serving(g.group) < g.min {
            return Err(Rejection::new(
                "group_limits",
                "Each node group must stay within its minimum and maximum size",
            ));
        }
    }
    if d.active_nodes() > d.node_budget {
        return Err(Rejection::new(
            "group_limits",
            "The cluster must stay within its node budget",
        ));
    }
    let provisioning = d.count(None, NodePhase::Provisioning);
    let draining = d.count(None, NodePhase::Draining);
    if !match phase {
        Phase::Stable => provisioning == 0 && draining == 0,
        Phase::ScalingUp => provisioning > 0 && draining == 0,
        Phase::ScalingDown => provisioning == 0 && draining == 1,
    } {
        return Err(Rejection::new(
            "phase_data",
            "The phase must agree with provisioning and draining nodes",
        ));
    }
    Ok(())
}

fn fresh(d: &Data, a: &Action, _: Instant) -> Result<(), Rejection> {
    if a.revision != d.revision {
        return Err(Rejection::new(
            "fresh",
            "The cluster changed while Jev was deciding",
        ));
    }
    if a.observed_at > d.at_ms || d.at_ms - a.observed_at > EVIDENCE_TTL_MS {
        return Err(Rejection::new(
            "fresh",
            "The evidence is more than 5 simulated seconds old",
        ));
    }
    Ok(())
}
fn within_limits(d: &Data, group: Group, count: usize) -> Result<(), Rejection> {
    let g = d.group(group);
    if d.active(group) + count > g.max {
        return Err(Rejection::new(
            "within_limits",
            format!("{} would exceed its maximum of {} nodes", g.name, g.max),
        ));
    }
    if d.active_nodes() + count > d.node_budget {
        return Err(Rejection::new(
            "within_limits",
            format!(
                "The cluster would exceed its budget of {} nodes",
                d.node_budget
            ),
        ));
    }
    Ok(())
}
fn justified_scale_up(d: &Data, headroom: Resources) -> Result<(), Rejection> {
    let (uncovered, spare) = coverage(d);
    let cpu = headroom.cpu.min(HEADROOM_CAP.cpu);
    let memory_gib = headroom.memory_gib.min(HEADROOM_CAP.memory_gib);
    if uncovered == 0 && spare.cpu >= cpu && spare.memory_gib >= memory_gib {
        return Err(Rejection::new(
            "justified_scale_up",
            "Ready and provisioning capacity already covers pending pods and forecast headroom",
        ));
    }
    Ok(())
}
fn scale_down_cooldown(d: &Data, group: Group) -> Result<(), Rejection> {
    if let Some(finished) = d
        .last_scale_up_finished_at
        .filter(|t| d.at_ms < t + SCALE_DOWN_COOLDOWN_MS)
    {
        return Err(Rejection::new(
            "scale_down_cooldown",
            format!(
                "A scale-up finished {}s ago; removals wait {}s",
                (d.at_ms - finished) / 1000,
                SCALE_DOWN_COOLDOWN_MS / 1000
            ),
        ));
    }
    if d.pending_count() > 0 {
        return Err(Rejection::new(
            "scale_down_cooldown",
            "Pods are pending; the cluster cannot shrink",
        ));
    }
    let g = d.group(group);
    if d.serving(group) <= g.min {
        return Err(Rejection::new(
            "scale_down_cooldown",
            format!("{} is at its minimum of {} nodes", g.name, g.min),
        ));
    }
    Ok(())
}
fn drainable(d: &Data, node: &Node) -> Result<(), Rejection> {
    if plan_drain(d, node.id).is_none() {
        return Err(Rejection::new(
            "drainable",
            format!("Pods on {} do not all fit on other ready nodes", node.name),
        ));
    }
    Ok(())
}
fn disruption_budget(d: &Data, node: &Node) -> Result<(), Rejection> {
    for (i, w) in d.workloads.iter().enumerate() {
        let on_node = |p: &&Pod| p.workload == i && p.node == Some(node.id);
        if d.pods.iter().filter(on_node).count() == 0 {
            continue;
        }
        let remaining = d
            .pods
            .iter()
            .filter(|p| p.workload == i && p.node != Some(node.id) && d.is_available(p))
            .count() as u32;
        if remaining < w.min_available() {
            return Err(Rejection::new(
                "disruption_budget",
                format!(
                    "Evicting from {} would leave {} with {remaining} available replicas; its minimum is {}",
                    node.name,
                    w.name,
                    w.min_available()
                ),
            ));
        }
    }
    Ok(())
}
fn can_scale_up(d: &Data, a: &Action, now: Instant) -> Result<(), Rejection> {
    let Choice::ScaleUp { group, count } = a.choice else {
        return Ok(());
    };
    fresh(d, a, now)?;
    within_limits(d, group, count as usize)?;
    if d.at_ms <= a.headroom_expires_at {
        return justified_scale_up(d, a.headroom);
    }
    // The forecast went stale while Jev was deciding: only pending pods can justify this now.
    justified_scale_up(d, Resources::default()).map_err(|rejection| {
        if a.headroom == Resources::default() {
            rejection
        } else {
            Rejection::new(
                "justified_scale_up",
                "The forecast behind this scale-up is no longer fresh, and no pending pods need it",
            )
        }
    })
}
fn can_remove(d: &Data, a: &Action, now: Instant) -> Result<(), Rejection> {
    let Choice::Remove { group, node } = a.choice else {
        return Ok(());
    };
    fresh(d, a, now)?;
    let node = d
        .node(node)
        .filter(|n| n.group == group && n.phase == NodePhase::Ready)
        .ok_or_else(|| Rejection::new("unknown_node", "The node is not a ready node"))?;
    scale_down_cooldown(d, group)?;
    drainable(d, node)?;
    disruption_budget(d, node)
}
/// Add an empty ready node to a group at the current time.
fn add_node(d: &mut Data, group: Group) -> &mut Node {
    d.next_node += 1;
    let g = d.group(group);
    let node = Node {
        id: d.next_node,
        name: node_name(group, d.next_node),
        group,
        phase: NodePhase::Ready,
        cpu: g.cpu,
        memory_gib: g.memory_gib,
        used_cpu: 0,
        used_memory_gib: 0,
        requested_at: d.at_ms,
        ready_at: Some(d.at_ms),
        removed_at: None,
        failed: false,
        operation: 0,
        due_at: None,
        doomed: false,
    };
    d.nodes.push(node);
    d.nodes.last_mut().expect("node just added")
}
fn scale_up(d: &mut Data, a: &Action, _: Instant) -> Result<(), Rejection> {
    let Choice::ScaleUp { group, count } = a.choice else {
        return Ok(());
    };
    if d.count(None, NodePhase::Provisioning) == 0 {
        d.operation += 1;
    }
    let doomed = d.group(group).stockout;
    for _ in 0..count {
        let (at, seed, operation) = (d.at_ms, d.seed, d.operation);
        let n = add_node(d, group);
        let jitter = draw(seed, 0x6e6f6465, n.id) % (2 * PROVISION_JITTER_MS + 1);
        n.phase = NodePhase::Provisioning;
        n.ready_at = None;
        n.operation = operation;
        // A stocked-out group refuses the request after a short wait.
        n.due_at = Some(if doomed {
            at + STOCKOUT_FAILURE_MS
        } else {
            at + PROVISION_MS - PROVISION_JITTER_MS + jitter
        });
        n.doomed = doomed;
    }
    d.last_change_at = Some(d.at_ms);
    d.revision += 1;
    Ok(())
}
fn remove(d: &mut Data, a: &Action, _: Instant) -> Result<(), Rejection> {
    let Choice::Remove { node, .. } = a.choice else {
        return Ok(());
    };
    let moves = plan_drain(d, node)
        .ok_or_else(|| Rejection::new("drainable", "Pods no longer fit on other ready nodes"))?;
    for (pod, target) in moves {
        release(d, pod);
        let target = d
            .nodes
            .iter()
            .position(|n| n.id == target)
            .expect("planned target");
        place(d, pod, target);
    }
    d.operation += 1;
    let operation = d.operation;
    let at = d.at_ms;
    let n = d
        .nodes
        .iter_mut()
        .find(|n| n.id == node)
        .expect("guarded node");
    n.phase = NodePhase::Draining;
    n.operation = operation;
    n.due_at = Some(at + DRAIN_MS);
    d.last_change_at = Some(at);
    d.revision += 1;
    Ok(())
}
/// The node an event names must be part of the operation in progress, and be due.
fn current_node(d: &Data, id: u64, phase: NodePhase) -> Result<&Node, Rejection> {
    d.node(id)
        .filter(|n| {
            n.phase == phase && n.operation == d.operation && n.due_at.is_some_and(|t| t <= d.at_ms)
        })
        .ok_or_else(|| {
            Rejection::new(
                "operation",
                "The node does not belong to the current operation",
            )
        })
}
fn current_provisioning(d: &Data, e: &Event, _: Instant) -> Result<(), Rejection> {
    let (Event::NodeReady { node, last } | Event::ProvisioningFailed { node, last }) = e else {
        return Ok(());
    };
    let n = current_node(d, *node, NodePhase::Provisioning)?;
    if n.doomed != matches!(e, Event::ProvisioningFailed { .. })
        || *last != (d.count(None, NodePhase::Provisioning) == 1)
    {
        return Err(Rejection::new(
            "operation",
            "The event does not match the node's provisioning result",
        ));
    }
    Ok(())
}
fn provisioning_finished(d: &mut Data, e: &Event, _: Instant) -> Result<(), Rejection> {
    let at = d.at_ms;
    match e {
        Event::NodeReady { node, .. } => {
            let n = d.nodes.iter_mut().find(|n| n.id == *node).unwrap();
            n.phase = NodePhase::Ready;
            n.ready_at = Some(at);
            n.due_at = None;
            d.last_scale_up_finished_at = Some(at);
            schedule(d);
        }
        Event::ProvisioningFailed { node, .. } => {
            let n = d.nodes.iter_mut().find(|n| n.id == *node).unwrap();
            n.phase = NodePhase::Removed;
            n.removed_at = Some(at);
            n.failed = true;
            n.due_at = None;
            let g = n.group.index();
            d.groups[g].failures += 1;
            d.groups[g].last_failure_at = Some(at);
        }
        _ => return Ok(()),
    }
    d.revision += 1;
    Ok(())
}
fn current_drain(d: &Data, e: &Event, _: Instant) -> Result<(), Rejection> {
    let Event::NodeDrained { node } = e else {
        return Ok(());
    };
    current_node(d, *node, NodePhase::Draining).map(|_| ())
}
fn node_drained(d: &mut Data, e: &Event, _: Instant) -> Result<(), Rejection> {
    if let Event::NodeDrained { node } = e {
        let at = d.at_ms;
        let n = d.nodes.iter_mut().find(|n| n.id == *node).unwrap();
        n.phase = NodePhase::Removed;
        n.removed_at = Some(at);
        n.due_at = None;
        d.revision += 1;
    }
    Ok(())
}
fn clock(d: &mut Data, e: &Event, _: Instant) -> Result<(), Rejection> {
    let Event::Clock(at) = e else { return Ok(()) };
    if *at < d.at_ms {
        return Err(Rejection::new("clock", "Time cannot go backward"));
    }
    let elapsed = at - d.at_ms;
    for g in Group::ALL {
        d.node_ms[g.index()] += d.active(g) as u64 * elapsed;
    }
    d.pending_pod_ms += d.pending_count() as u64 * elapsed;
    d.at_ms = *at;
    Ok(())
}
fn known_target(d: &Data, e: &Event, _: Instant) -> Result<(), Rejection> {
    let workload = match e {
        Event::Demand { workload, replicas } if *replicas > MAX_REPLICAS => {
            return Err(Rejection::new(
                "unknown_target",
                format!("A workload wants at most {MAX_REPLICAS} replicas before any surge"),
            ))
        }
        Event::Demand { workload, .. }
        | Event::Control(
            Control::ReplicaSurge { workload, .. } | Control::MemoryHeavy { workload, .. },
        ) => *workload,
        _ => return Ok(()),
    };
    if workload >= d.workloads.len() {
        return Err(Rejection::new("unknown_target", "Unknown workload"));
    }
    Ok(())
}
/// Load and operator controls. Placement reruns in the same commit as the change.
fn demand(d: &mut Data, e: &Event, _: Instant) -> Result<(), Rejection> {
    match *e {
        Event::Demand { workload, replicas } => {
            if d.workloads[workload].replicas == replicas {
                return Ok(());
            }
            d.workloads[workload].replicas = replicas;
            reconcile(d, workload);
        }
        Event::Control(Control::ReplicaSurge { workload, enabled }) => {
            if d.workloads[workload].surge == enabled {
                return Ok(());
            }
            d.workloads[workload].surge = enabled;
            reconcile(d, workload);
        }
        Event::Control(Control::MemoryHeavy { workload, enabled }) => {
            if d.workloads[workload].memory_heavy == enabled {
                return Ok(());
            }
            d.workloads[workload].memory_heavy = enabled;
            // A new pod size recreates the workload's pods: each must find a node again.
            let memory_gib = d.workloads[workload].pod_memory_gib();
            for pod in 0..d.pods.len() {
                if d.pods[pod].workload == workload {
                    release(d, pod);
                    d.pods[pod].memory_gib = memory_gib;
                    d.pods[pod].pending_since = Some(d.at_ms);
                    d.pods[pod].available_at = None;
                }
            }
        }
        // Stock is a property of the provider, not of the cluster Jev observed: the
        // revision is left alone and only later provisioning requests are affected.
        Event::Control(Control::Stockout { group, enabled }) => {
            d.groups[group.index()].stockout = enabled;
            return Ok(());
        }
        _ => return Ok(()),
    }
    schedule(d);
    d.revision += 1;
    Ok(())
}

/// Structurally possible actions. Whether a scale-up is justified or a removal is safe is
/// left to Jev's judgment and re-checked by the guards when the recommendation executes.
pub fn legal_choices(phase: Phase, d: &Data) -> Vec<Choice> {
    let mut choices = vec![Choice::NoChange];
    if phase != Phase::ScalingDown {
        for group in Group::ALL {
            for count in [1, 2] {
                if within_limits(d, group, count as usize).is_ok() {
                    choices.push(Choice::ScaleUp { group, count });
                }
            }
        }
    }
    if phase == Phase::Stable {
        for n in &d.nodes {
            if n.phase == NodePhase::Ready && d.serving(n.group) > d.group(n.group).min {
                choices.push(Choice::Remove {
                    group: n.group,
                    node: n.id,
                });
            }
        }
    }
    choices
}

fn initial(seed: u64) -> Data {
    let groups = [
        (Group::GeneralSmall, "Small general", 4, 8, 1, 4, 0.2),
        (Group::GeneralLarge, "Large general", 8, 16, 1, 5, 0.4),
        (Group::MemoryHeavy, "Memory-heavy", 8, 64, 0, 3, 0.6),
    ]
    .into_iter()
    .map(
        |(group, name, cpu, memory_gib, min, max, hourly_usd)| NodeGroup {
            group,
            name,
            cpu,
            memory_gib,
            min,
            max,
            hourly_usd,
            stockout: false,
            failures: 0,
            last_failure_at: None,
        },
    )
    .collect();
    let workloads = [(2, 5, 4), (3, 6, 2), (4, 8, 1)]
        .into_iter()
        .zip(WORKLOADS)
        .map(|((cpu, memory_gib, replicas), name)| Workload {
            name,
            cpu,
            memory_gib,
            replicas,
            surge: false,
            memory_heavy: false,
        })
        .collect();
    let mut d = Data {
        at_ms: 0,
        revision: 0,
        seed,
        node_budget: NODE_BUDGET,
        workloads,
        groups,
        nodes: vec![],
        pods: vec![],
        last_change_at: None,
        last_scale_up_finished_at: None,
        node_ms: [0; 3],
        pending_pod_ms: 0,
        operation: 0,
        next_node: 0,
        next_pod: 0,
    };
    for group in [
        Group::GeneralSmall,
        Group::GeneralSmall,
        Group::GeneralLarge,
        Group::GeneralLarge,
    ] {
        add_node(&mut d, group);
    }
    for workload in 0..d.workloads.len() {
        reconcile(&mut d, workload);
    }
    schedule(&mut d);
    // The run starts with every replica already serving.
    for p in &mut d.pods {
        p.available_at = p.node.map(|_| 0);
    }
    d
}

pub struct Engine {
    machine: StateMachineExecutor<Phase, Data, Action, Event>,
    log: Vec<Transition>,
}
impl Engine {
    pub fn new(seed: u64) -> Result<Self, Error> {
        Self::from_state(Phase::Stable, initial(seed), None)
    }
    /// Count this machine's `reflex.transitions` on an application-owned meter. Without one
    /// the executor uses the global provider, like the other simulations.
    pub fn with_meter(seed: u64, meter: opentelemetry::metrics::Meter) -> Result<Self, Error> {
        Self::from_state(Phase::Stable, initial(seed), Some(meter))
    }
    fn from_state(
        phase: Phase,
        data: Data,
        meter: Option<opentelemetry::metrics::Meter>,
    ) -> Result<Self, Error> {
        let definition = state_machine! {
            phase: Phase, data: Data, action: Action, event: Event, invariants: [invariant],
            transitions: [
                Phase::Stable + action(Action { choice: Choice::ScaleUp { .. }, .. }) => Phase::ScalingUp { guard: can_scale_up, update: scale_up },
                Phase::ScalingUp + action(Action { choice: Choice::ScaleUp { .. }, .. }) => Phase::ScalingUp { guard: can_scale_up, update: scale_up },
                Phase::ScalingUp + event(Event::NodeReady { last: false, .. } | Event::ProvisioningFailed { last: false, .. }) => Phase::ScalingUp { guard: current_provisioning, update: provisioning_finished },
                Phase::ScalingUp + event(Event::NodeReady { last: true, .. } | Event::ProvisioningFailed { last: true, .. }) => Phase::Stable { guard: current_provisioning, update: provisioning_finished },
                Phase::Stable + action(Action { choice: Choice::Remove { .. }, .. }) => Phase::ScalingDown { guard: can_remove, update: remove },
                Phase::ScalingDown + event(Event::NodeDrained { .. }) => Phase::Stable { guard: current_drain, update: node_drained },
                Phase::Stable + action(Action { choice: Choice::NoChange, .. }) => Phase::Stable { guard: fresh },
                Phase::ScalingUp + action(Action { choice: Choice::NoChange, .. }) => Phase::ScalingUp { guard: fresh },
                Phase::ScalingDown + action(Action { choice: Choice::NoChange, .. }) => Phase::ScalingDown { guard: fresh },
                Phase::Stable + evaluation_error(_) => unchanged {},
                Phase::ScalingUp + evaluation_error(_) => unchanged {},
                Phase::ScalingDown + evaluation_error(_) => unchanged {},
                Phase::Stable + event(Event::Clock(_)) => Phase::Stable { update: clock },
                Phase::ScalingUp + event(Event::Clock(_)) => Phase::ScalingUp { update: clock },
                Phase::ScalingDown + event(Event::Clock(_)) => Phase::ScalingDown { update: clock },
                Phase::Stable + event(Event::Demand { .. } | Event::Control(_)) => Phase::Stable { guard: known_target, update: demand },
                Phase::ScalingUp + event(Event::Demand { .. } | Event::Control(_)) => Phase::ScalingUp { guard: known_target, update: demand },
                Phase::ScalingDown + event(Event::Demand { .. } | Event::Control(_)) => Phase::ScalingDown { guard: known_target, update: demand },
            ],
        };
        let mut builder = StateMachineExecutor::builder(definition)
            .name("cluster_autoscaler")
            .store(InMemory::new(phase, data));
        if let Some(meter) = meter {
            builder = builder.meter(meter);
        }
        let machine = builder.build().map_err(|e| Error::Policy(e.to_string()))?;
        Ok(Self {
            machine,
            log: vec![],
        })
    }
    pub fn state(&self) -> (Phase, Data) {
        self.machine.state().expect("in-memory state")
    }
    pub fn phase(&self) -> Phase {
        self.state().0
    }
    pub fn data(&self) -> Data {
        self.state().1
    }
    /// Phase changes and node lifecycle steps since the last call, in order.
    pub fn take_log(&mut self) -> Vec<Transition> {
        std::mem::take(&mut self.log)
    }
    async fn event(&mut self, e: Event) -> Result<(), Error> {
        match self
            .machine
            .handle_event(e)
            .await
            .map_err(|e| Error::Policy(e.to_string()))?
        {
            ExecutionOutcome::Applied(_) => Ok(()),
            ExecutionOutcome::Rejected { reason, .. } => Err(Error::Policy(format!(
                "{}: {}",
                reason.code, reason.message
            ))),
        }
    }
    /// Move the clock, completing provisioning and drains at their exact due times.
    pub async fn advance(&mut self, at: u64) -> Result<(), Error> {
        loop {
            let (from, d) = self.state();
            let provisioning = d.count(None, NodePhase::Provisioning);
            let Some(n) = d
                .nodes
                .iter()
                .filter(|n| n.due_at.is_some_and(|t| t <= at))
                .min_by_key(|n| (n.due_at, n.id))
            else {
                break;
            };
            let due = n.due_at.unwrap_or(at).max(d.at_ms);
            let last = provisioning == 1;
            let (event, trigger) = match n.phase {
                NodePhase::Provisioning if n.doomed => (
                    Event::ProvisioningFailed { node: n.id, last },
                    format!(
                        "{} failed to provision: {} is out of stock",
                        n.name,
                        d.group(n.group).name
                    ),
                ),
                NodePhase::Provisioning => (
                    Event::NodeReady { node: n.id, last },
                    format!("{} ready", n.name),
                ),
                _ => (
                    Event::NodeDrained { node: n.id },
                    format!("{} drained and removed", n.name),
                ),
            };
            self.event(Event::Clock(due)).await?;
            self.event(event).await?;
            self.log.push(Transition {
                at_ms: due,
                from,
                to: self.phase(),
                trigger,
            });
        }
        self.event(Event::Clock(at)).await
    }
    pub async fn set_replicas(&mut self, workload: usize, replicas: u32) -> Result<(), Error> {
        self.event(Event::Demand { workload, replicas }).await
    }
    pub async fn control(&mut self, control: Control) -> Result<(), Error> {
        self.event(Event::Control(control)).await
    }
    pub async fn evaluation_error(&mut self, code: &str, message: &str) -> Result<Outcome, Error> {
        let from = self.phase();
        self.machine
            .execute(Err(reflex::EvaluationError::Judge(
                reflex::JudgeError::new(code, message),
            )))
            .await
            .map_err(|e| Error::Policy(e.to_string()))?;
        Ok(Outcome {
            status: "evaluation_error",
            code: Some(code.into()),
            reason: format!("{message}. The cluster is unchanged"),
            from,
            to: self.phase(),
        })
    }
    pub async fn apply(
        &mut self,
        action: Action,
        confidence: Option<f64>,
    ) -> Result<Outcome, Error> {
        let from = self.phase();
        let result = self
            .machine
            .execute(Judgment { action, confidence }.try_into())
            .await
            .map_err(|e| Error::Policy(e.to_string()))?;
        let (to, d) = self.state();
        let (status, code, reason) = match result {
            ExecutionOutcome::Applied(r) if r.evaluation_error.is_some() => (
                "evaluation_error",
                Some("invalid_judgment".to_owned()),
                "Invalid judgment. The cluster is unchanged".to_owned(),
            ),
            ExecutionOutcome::Applied(_) => match action.choice {
                Choice::NoChange => (
                    "unchanged",
                    None,
                    "fresh passed; Jev recommended no change".to_owned(),
                ),
                Choice::ScaleUp { group, count } => {
                    self.log.push(Transition {
                        at_ms: d.at_ms,
                        from,
                        to,
                        trigger: format!("Jev: scale up {} by {count}", d.group(group).name),
                    });
                    (
                        "applied",
                        None,
                        "fresh, within_limits and justified_scale_up passed".to_owned(),
                    )
                }
                Choice::Remove { group, node } => {
                    self.log.push(Transition {
                        at_ms: d.at_ms,
                        from,
                        to,
                        trigger: format!("Jev: remove {}", node_name(group, node)),
                    });
                    (
                        "applied",
                        None,
                        "fresh, scale_down_cooldown, drainable and disruption_budget passed"
                            .to_owned(),
                    )
                }
            },
            ExecutionOutcome::Rejected { reason, .. } => (
                "rejected",
                Some(reason.code.clone()),
                format!("{}: {}", reason.code, reason.message),
            ),
        };
        Ok(Outcome {
            status,
            code,
            reason,
            from,
            to,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WEB: usize = 0;
    const API: usize = 1;
    const BATCH: usize = 2;

    fn act(d: &Data, choice: Choice) -> Action {
        Action {
            choice,
            revision: d.revision,
            observed_at: d.at_ms,
            headroom: Resources::default(),
            headroom_expires_at: u64::MAX,
        }
    }
    fn up(group: Group, count: u8) -> Choice {
        Choice::ScaleUp { group, count }
    }
    fn remove_node(n: &Node) -> Choice {
        Choice::Remove {
            group: n.group,
            node: n.id,
        }
    }
    fn snapshot(e: &Engine) -> serde_json::Value {
        serde_json::to_value(e.data()).unwrap()
    }
    /// The executor checks invariants itself; this makes each committed state explicit.
    fn check(e: &Engine) {
        let (phase, d) = e.state();
        invariant(&phase, &d).unwrap();
    }
    async fn apply(e: &mut Engine, choice: Choice) -> Outcome {
        let action = act(&e.data(), choice);
        let outcome = e.apply(action, Some(0.9)).await.unwrap();
        check(e);
        outcome
    }
    /// Ready nodes of the given groups, holding the listed `(workload, node index)` pods.
    fn cluster(groups: &[Group], pods: &[(usize, Option<usize>)]) -> Data {
        let mut d = initial(42);
        d.nodes.clear();
        d.pods.clear();
        (d.next_node, d.next_pod) = (0, 0);
        for w in &mut d.workloads {
            w.replicas = 0;
        }
        for group in groups {
            add_node(&mut d, *group);
        }
        for (workload, node) in pods {
            d.workloads[*workload].replicas += 1;
            reconcile(&mut d, *workload);
            if let Some(node) = node {
                let pod = d.pods.len() - 1;
                place(&mut d, pod, *node);
                d.pods[pod].available_at = Some(0);
            }
        }
        d
    }
    fn on_node<'a>(d: &'a Data, name: &str) -> Vec<&'a str> {
        let id = d.nodes.iter().find(|n| n.name == name).unwrap().id;
        let mut pods: Vec<_> = d
            .pods
            .iter()
            .filter(|p| p.node == Some(id))
            .map(|p| d.workloads[p.workload].name)
            .collect();
        pods.sort_unstable();
        pods
    }
    async fn rejected(e: &Engine, event: Event) -> String {
        match e.machine.handle_event(event).await.unwrap() {
            ExecutionOutcome::Rejected { reason, .. } => reason.code,
            ExecutionOutcome::Applied(_) => panic!("event accepted"),
        }
    }

    #[test]
    fn initial_cluster_is_packed_by_best_fit() {
        let d = initial(42);
        invariant(&Phase::Stable, &d).unwrap();
        assert_eq!(d.pending_count(), 0);
        assert_eq!(on_node(&d, "small-1"), ["web"]);
        assert_eq!(on_node(&d, "small-2"), ["web"]);
        // The tightest fit wins: large-3 fills before large-4 opens.
        assert_eq!(on_node(&d, "large-3"), ["api", "web", "web"]);
        assert_eq!(on_node(&d, "large-4"), ["api", "batch"]);
        for (i, w) in d.workloads.iter().enumerate() {
            assert_eq!(d.available(i), w.desired());
        }
        let requested = d.requested();
        assert_eq!((requested.cpu, requested.memory_gib), (18, 40));
        // Placement does not depend on the seed.
        assert_eq!(
            serde_json::to_value(&initial(7).pods).unwrap(),
            serde_json::to_value(&d.pods).unwrap()
        );
    }

    #[test]
    fn best_fit_prefers_the_tightest_ready_node_and_skips_other_phases() {
        let groups = [Group::GeneralLarge, Group::GeneralSmall, Group::MemoryHeavy];
        let mut d = cluster(&groups, &[]);
        assert_eq!(best_fit(&d.nodes, 2, 5), Some(1));
        assert_eq!(best_fit(&d.nodes, 4, 9), Some(0));
        assert_eq!(best_fit(&d.nodes, 3, 24), Some(2));
        assert_eq!(best_fit(&d.nodes, 9, 1), None);
        d.nodes[1].phase = NodePhase::Provisioning;
        assert_eq!(best_fit(&d.nodes, 2, 5), Some(0));
        d.nodes[0].phase = NodePhase::Draining;
        assert_eq!(best_fit(&d.nodes, 2, 5), Some(2));
        d.nodes[2].phase = NodePhase::Removed;
        assert_eq!(best_fit(&d.nodes, 2, 5), None);
    }

    #[test]
    fn memory_heavy_pods_fit_only_the_memory_heavy_group() {
        let d = initial(42);
        for w in &d.workloads {
            let memory_gib = w.memory_gib * MEMORY_HEAVY_FACTOR;
            for g in &d.groups {
                let fits = w.cpu <= g.cpu && memory_gib <= g.memory_gib;
                assert_eq!(
                    fits,
                    g.group == Group::MemoryHeavy,
                    "{} on {}",
                    w.name,
                    g.name
                );
                assert!(w.cpu <= g.cpu && w.memory_gib <= g.memory_gib);
            }
        }
    }

    #[tokio::test]
    async fn the_wrong_node_group_leaves_pods_pending() {
        let mut e = Engine::new(42).unwrap();
        let heavy = Control::MemoryHeavy {
            workload: API,
            enabled: true,
        };
        e.control(heavy).await.unwrap();
        check(&e);
        let d = e.data();
        assert_eq!(d.pending_count(), 2);
        assert!(d
            .pending()
            .iter()
            .all(|p| p.workload == API && p.memory_gib == 24));
        assert_eq!(d.available(API), 0);
        assert_eq!(d.available(WEB), 4);

        let outcome = apply(&mut e, up(Group::GeneralLarge, 1)).await;
        assert_eq!(outcome.status, "applied");
        e.advance(40_000).await.unwrap();
        check(&e);
        assert_eq!(e.phase(), Phase::Stable);
        let d = e.data();
        assert_eq!(d.count(Some(Group::GeneralLarge), NodePhase::Ready), 3);
        assert_eq!(d.pending_count(), 2, "a general node cannot hold them");
        assert_eq!(d.oldest_pending_ms(), 40_000);

        let outcome = apply(&mut e, up(Group::MemoryHeavy, 1)).await;
        assert_eq!(outcome.status, "applied");
        e.advance(80_000).await.unwrap();
        check(&e);
        let d = e.data();
        assert_eq!(d.pending_count(), 0);
        assert_eq!(on_node(&d, "mem-6"), ["api", "api"]);
        // Replicas start on their new node before they serve again.
        let ready = d.node(6).unwrap().ready_at.unwrap();
        assert!(d
            .pods
            .iter()
            .filter(|p| p.workload == API)
            .all(|p| p.available_at == Some(ready + POD_START_MS)));
        assert_eq!(d.available(API), 2);
    }

    #[tokio::test]
    async fn fresh_rejects_a_changed_revision_and_old_or_future_evidence() {
        let mut e = Engine::new(42).unwrap();
        let before_surge = act(&e.data(), up(Group::GeneralLarge, 1));
        let surge = Control::ReplicaSurge {
            workload: WEB,
            enabled: true,
        };
        e.control(surge).await.unwrap();
        let before = snapshot(&e);
        let outcome = e.apply(before_surge, Some(1.)).await.unwrap();
        assert_eq!(outcome.status, "rejected");
        assert_eq!(outcome.code.as_deref(), Some("fresh"));
        assert!(outcome.reason.contains("changed while Jev was deciding"));
        assert_eq!(before, snapshot(&e));

        let old = act(&e.data(), up(Group::GeneralLarge, 1));
        e.advance(5_001).await.unwrap();
        let before = snapshot(&e);
        let outcome = e.apply(old, Some(1.)).await.unwrap();
        assert_eq!(outcome.code.as_deref(), Some("fresh"));
        assert!(outcome.reason.contains("5 simulated seconds"));
        // The same age limit covers a recommendation to do nothing.
        let no_change = Action {
            choice: Choice::NoChange,
            ..old
        };
        let outcome = e.apply(no_change, None).await.unwrap();
        assert_eq!(outcome.code.as_deref(), Some("fresh"));
        let mut future = act(&e.data(), up(Group::GeneralLarge, 1));
        future.observed_at += 1;
        let outcome = e.apply(future, None).await.unwrap();
        assert_eq!(outcome.code.as_deref(), Some("fresh"));
        assert_eq!(before, snapshot(&e));

        let current = act(&e.data(), up(Group::GeneralLarge, 1));
        e.advance(10_001).await.unwrap();
        let outcome = e.apply(current, None).await.unwrap();
        assert_eq!(
            outcome.status, "applied",
            "exactly five seconds old is fresh"
        );
        assert_eq!(
            (outcome.from, outcome.to),
            (Phase::Stable, Phase::ScalingUp)
        );
        check(&e);
    }

    #[tokio::test]
    async fn within_limits_rejects_a_full_group_and_an_exhausted_node_budget() {
        let mut e = Engine::new(42).unwrap();
        for workload in [WEB, API, BATCH] {
            let surge = Control::ReplicaSurge {
                workload,
                enabled: true,
            };
            e.control(surge).await.unwrap();
        }
        assert_eq!(e.data().pending_count(), 14);
        let outcome = apply(&mut e, up(Group::GeneralLarge, 2)).await;
        assert_eq!(outcome.status, "applied");
        let before = snapshot(&e);
        let outcome = apply(&mut e, up(Group::GeneralLarge, 2)).await;
        assert_eq!(outcome.code.as_deref(), Some("within_limits"));
        assert!(outcome
            .reason
            .contains("Large general would exceed its maximum of 5"));
        assert_eq!(before, snapshot(&e));
        let outcome = apply(&mut e, up(Group::GeneralLarge, 1)).await;
        assert_eq!(outcome.status, "applied");
        let outcome = apply(&mut e, up(Group::GeneralSmall, 2)).await;
        assert_eq!(outcome.status, "applied");
        assert_eq!(e.data().active_nodes(), 9);
        let before = snapshot(&e);
        let outcome = apply(&mut e, up(Group::MemoryHeavy, 2)).await;
        assert_eq!(outcome.code.as_deref(), Some("within_limits"));
        assert!(outcome.reason.contains("budget of 10 nodes"));
        assert_eq!(before, snapshot(&e));
        let outcome = apply(&mut e, up(Group::MemoryHeavy, 1)).await;
        assert_eq!(outcome.status, "applied");
        let (phase, d) = e.state();
        assert_eq!(d.active_nodes(), NODE_BUDGET);
        assert_eq!(legal_choices(phase, &d), [Choice::NoChange]);
    }

    #[tokio::test]
    async fn justified_scale_up_needs_uncovered_pods_or_capped_forecast_headroom() {
        let mut e = Engine::new(42).unwrap();
        let before = snapshot(&e);
        let outcome = apply(&mut e, up(Group::GeneralLarge, 1)).await;
        assert_eq!(outcome.status, "rejected");
        assert_eq!(outcome.code.as_deref(), Some("justified_scale_up"));
        assert_eq!(before, snapshot(&e));

        // A forecast justifies provisioning ahead of demand, up to what it asks for.
        let headroom = |e: &Engine, cpu, memory_gib| Action {
            headroom: Resources { cpu, memory_gib },
            ..act(&e.data(), up(Group::GeneralLarge, 1))
        };
        let outcome = e.apply(headroom(&e, 4, 8), None).await.unwrap();
        assert_eq!(outcome.status, "applied");
        let outcome = e.apply(headroom(&e, 4, 8), None).await.unwrap();
        assert_eq!(
            outcome.code.as_deref(),
            Some("justified_scale_up"),
            "the provisioning node already covers it"
        );
        // However large the forecast, headroom is capped at three large nodes.
        for _ in 0..2 {
            let outcome = e.apply(headroom(&e, 100, 100), None).await.unwrap();
            assert_eq!(outcome.status, "applied");
        }
        let small = Action {
            choice: up(Group::GeneralSmall, 1),
            ..headroom(&e, 100, 100)
        };
        let outcome = e.apply(small, None).await.unwrap();
        assert_eq!(outcome.code.as_deref(), Some("justified_scale_up"));
        assert_eq!(e.data().count(None, NodePhase::Provisioning), 3);
        check(&e);

        // Pending pods justify capacity until provisioning nodes cover them.
        let mut e = Engine::new(42).unwrap();
        let surge = Control::ReplicaSurge {
            workload: BATCH,
            enabled: true,
        };
        e.control(surge).await.unwrap();
        assert_eq!(e.data().pending_count(), 2);
        let outcome = apply(&mut e, up(Group::GeneralLarge, 1)).await;
        assert_eq!(outcome.status, "applied");
        let outcome = apply(&mut e, up(Group::GeneralLarge, 1)).await;
        assert_eq!(outcome.code.as_deref(), Some("justified_scale_up"));
        // Spare fragments too small for any pod do not count as cover.
        let d = initial(42);
        assert_eq!(coverage(&d), (0, Resources::default()));
        assert!(d.nodes.iter().map(|n| n.free().cpu).sum::<u32>() > 0);
    }

    #[tokio::test]
    async fn a_forecast_that_expired_while_jev_decided_justifies_nothing() {
        let forecast = |e: &Engine, expires_at| Action {
            headroom: Resources {
                cpu: 12,
                memory_gib: 20,
            },
            headroom_expires_at: expires_at,
            ..act(&e.data(), up(Group::GeneralLarge, 1))
        };
        let mut e = Engine::new(42).unwrap();
        e.advance(10_000).await.unwrap();
        let before = snapshot(&e);
        let outcome = e.apply(forecast(&e, 9_999), None).await.unwrap();
        assert_eq!(outcome.status, "rejected");
        assert_eq!(outcome.code.as_deref(), Some("justified_scale_up"));
        assert!(outcome.reason.contains("no longer fresh"));
        assert_eq!(before, snapshot(&e));
        // Up to its limit, the same forecast justifies the node.
        let outcome = e.apply(forecast(&e, 10_000), None).await.unwrap();
        assert_eq!(outcome.status, "applied");
        check(&e);

        // Pending pods need no forecast: an expired one does not get in their way.
        let mut e = Engine::new(42).unwrap();
        let surge = Control::ReplicaSurge {
            workload: WEB,
            enabled: true,
        };
        e.control(surge).await.unwrap();
        e.advance(4_000).await.unwrap();
        let outcome = e.apply(forecast(&e, 0), None).await.unwrap();
        assert_eq!(outcome.status, "applied");
    }

    #[test]
    fn scale_down_cooldown_waits_for_scale_ups_pending_pods_and_group_minimums() {
        let large = Group::GeneralLarge;
        let pods = [(WEB, Some(0)), (WEB, Some(1))];
        let mut d = cluster(&[large, large, large, Group::GeneralSmall], &pods);
        assert!(scale_down_cooldown(&d, large).is_ok());

        d.last_scale_up_finished_at = Some(1_000);
        d.at_ms = 1_000 + SCALE_DOWN_COOLDOWN_MS - 1;
        let rejection = scale_down_cooldown(&d, large).unwrap_err();
        assert_eq!(rejection.code, "scale_down_cooldown");
        assert!(rejection.message.contains("scale-up finished 29s ago"));
        d.at_ms += 1;
        assert!(scale_down_cooldown(&d, large).is_ok());

        // A replica that fits nowhere blocks any removal.
        d.workloads[BATCH].memory_heavy = true;
        d.workloads[BATCH].replicas = 1;
        reconcile(&mut d, BATCH);
        schedule(&mut d);
        let rejection = scale_down_cooldown(&d, large).unwrap_err();
        assert_eq!(rejection.code, "scale_down_cooldown");
        assert!(rejection.message.contains("pending"));

        let d = cluster(&[large, Group::GeneralSmall], &[]);
        let rejection = scale_down_cooldown(&d, large).unwrap_err();
        assert_eq!(rejection.code, "scale_down_cooldown");
        assert!(rejection.message.contains("minimum of 1"));
        // A group at its minimum is not offered for removal either.
        let offered = legal_choices(Phase::Stable, &d);
        assert!(!offered.iter().any(|c| matches!(c, Choice::Remove { .. })));
    }

    #[tokio::test]
    async fn drainable_requires_every_evicted_pod_to_fit_on_another_ready_node() {
        let large = Group::GeneralLarge;
        let small = Group::GeneralSmall;
        // Both large nodes are full and the small node has too little room for a batch pod.
        let pods = [
            (BATCH, Some(0)),
            (BATCH, Some(0)),
            (BATCH, Some(1)),
            (BATCH, Some(1)),
            (WEB, Some(2)),
        ];
        let d = cluster(&[large, large, small], &pods);
        assert_eq!(drainable(&d, &d.nodes[0]).unwrap_err().code, "drainable");
        assert_eq!(drainable(&d, &d.nodes[1]).unwrap_err().code, "drainable");
        let target = d.nodes[0].clone();
        let mut e = Engine::from_state(Phase::Stable, d, None).unwrap();
        let before = snapshot(&e);
        let outcome = apply(&mut e, remove_node(&target)).await;
        assert_eq!(outcome.status, "rejected");
        assert_eq!(outcome.code.as_deref(), Some("drainable"));
        assert!(outcome.reason.contains("large-1"));
        assert_eq!(before, snapshot(&e));
        assert_eq!(e.phase(), Phase::Stable);

        let pods = [(WEB, Some(0)), (WEB, Some(2))];
        let d = cluster(&[large, large, small, small], &pods);
        let target = d.nodes[2].clone();
        assert!(drainable(&d, &target).is_ok());
        let mut e = Engine::from_state(Phase::Stable, d, None).unwrap();
        e.advance(1_000).await.unwrap();
        let outcome = apply(&mut e, remove_node(&target)).await;
        assert_eq!(outcome.status, "applied");
        assert_eq!(
            (outcome.from, outcome.to),
            (Phase::Stable, Phase::ScalingDown)
        );
        let d = e.data();
        // The evicted pod moves in the same commit, to the tightest fit, and restarts.
        assert_eq!(on_node(&d, "large-1"), ["web", "web"]);
        assert_eq!(d.nodes[2].phase, NodePhase::Draining);
        assert_eq!((d.nodes[2].used_cpu, d.nodes[2].used_memory_gib), (0, 0));
        assert_eq!(d.available(WEB), 1);
        // A draining node takes no new pods.
        e.set_replicas(WEB, 3).await.unwrap();
        check(&e);
        assert!(on_node(&e.data(), "small-3").is_empty());
        e.advance(1_000 + DRAIN_MS - 1).await.unwrap();
        assert_eq!(e.phase(), Phase::ScalingDown);
        e.advance(1_000 + DRAIN_MS).await.unwrap();
        check(&e);
        assert_eq!(e.phase(), Phase::Stable);
        let d = e.data();
        assert_eq!(d.nodes[2].phase, NodePhase::Removed);
        assert_eq!(d.nodes[2].removed_at, Some(11_000));
        assert_eq!(d.active_nodes(), 3);
    }

    #[tokio::test]
    async fn disruption_budget_keeps_half_of_each_workload_available() {
        let large = Group::GeneralLarge;
        let pods = [
            (WEB, Some(0)),
            (WEB, Some(0)),
            (WEB, Some(0)),
            (WEB, Some(1)),
        ];
        let d = cluster(&[large, large, large, Group::GeneralSmall], &pods);
        assert!(drainable(&d, &d.nodes[0]).is_ok());
        let rejection = disruption_budget(&d, &d.nodes[0]).unwrap_err();
        assert_eq!(rejection.code, "disruption_budget");
        assert!(rejection
            .message
            .contains("leave web with 1 available replicas; its minimum is 2"));
        assert!(disruption_budget(&d, &d.nodes[1]).is_ok());
        assert!(disruption_budget(&d, &d.nodes[2]).is_ok());

        // Replicas that are still starting do not count as available.
        let mut starting = d.clone();
        starting.pods[0].available_at = Some(1);
        starting.pods[1].available_at = Some(1);
        let rejection = disruption_budget(&starting, &starting.nodes[1]).unwrap_err();
        assert_eq!(rejection.code, "disruption_budget");

        let (unsafe_node, safe_node) = (d.nodes[0].clone(), d.nodes[1].clone());
        let mut e = Engine::from_state(Phase::Stable, d, None).unwrap();
        let before = snapshot(&e);
        let outcome = apply(&mut e, remove_node(&unsafe_node)).await;
        assert_eq!(outcome.status, "rejected");
        assert_eq!(outcome.code.as_deref(), Some("disruption_budget"));
        assert_eq!(before, snapshot(&e));
        // Confidence is not a way past a guard.
        let action = act(&e.data(), remove_node(&unsafe_node));
        assert_eq!(e.apply(action, Some(1.)).await.unwrap().status, "rejected");
        let outcome = apply(&mut e, remove_node(&safe_node)).await;
        assert_eq!(outcome.status, "applied");
        assert_eq!(e.phase(), Phase::ScalingDown);
    }

    #[tokio::test]
    async fn removal_targets_must_be_ready_nodes_of_the_named_group() {
        let large = Group::GeneralLarge;
        let d = cluster(&[large, large, large, Group::GeneralSmall], &[]);
        let mut e = Engine::from_state(Phase::Stable, d, None).unwrap();
        let unknown = Choice::Remove {
            group: large,
            node: 99,
        };
        let mismatched = Choice::Remove {
            group: Group::MemoryHeavy,
            node: 1,
        };
        for choice in [unknown, mismatched] {
            let outcome = apply(&mut e, choice).await;
            assert_eq!(outcome.code.as_deref(), Some("unknown_node"));
        }
        assert_eq!(e.data().active_nodes(), 4);
    }

    #[tokio::test]
    async fn lifecycle_events_must_belong_to_the_current_operation() {
        let mut e = Engine::new(42).unwrap();
        let surge = Control::ReplicaSurge {
            workload: WEB,
            enabled: true,
        };
        e.control(surge).await.unwrap();
        let outcome = apply(&mut e, up(Group::GeneralLarge, 2)).await;
        assert_eq!(outcome.status, "applied");
        assert_eq!(e.phase(), Phase::ScalingUp);
        let before = snapshot(&e);
        // Not due yet, unknown, the wrong result, and a node from no operation.
        for (node, failed) in [(5, false), (99, false), (5, true), (1, false)] {
            let event = if failed {
                Event::ProvisioningFailed { node, last: false }
            } else {
                Event::NodeReady { node, last: false }
            };
            assert_eq!(rejected(&e, event).await, "operation");
        }
        let drained = Event::NodeDrained { node: 5 };
        assert_eq!(rejected(&e, drained).await, "undefined_transition");
        assert_eq!(before, snapshot(&e));

        e.advance(24_999).await.unwrap();
        assert_eq!(e.data().count(None, NodePhase::Provisioning), 2);
        // Due, but claiming to be the last node while another is still provisioning.
        e.advance(35_000).await.unwrap();
        check(&e);
        let d = e.data();
        assert_eq!(e.phase(), Phase::Stable);
        assert_eq!(d.count(Some(Group::GeneralLarge), NodePhase::Ready), 4);
        assert_eq!(d.pending_count(), 2);
        let log = e.take_log();
        assert_eq!(log.len(), 3);
        assert_eq!(log[0].trigger, "Jev: scale up Large general by 2");
        assert_eq!(
            (log[1].from, log[1].to),
            (Phase::ScalingUp, Phase::ScalingUp)
        );
        assert_eq!((log[2].from, log[2].to), (Phase::ScalingUp, Phase::Stable));
        assert!(log[1].at_ms <= log[2].at_ms && log[1].at_ms >= 25_000);
        assert_eq!(d.last_scale_up_finished_at, Some(log[2].at_ms));
        // A finished operation cannot be completed twice.
        let again = Event::NodeReady {
            node: 5,
            last: true,
        };
        assert_eq!(rejected(&e, again).await, "undefined_transition");
    }

    #[tokio::test]
    async fn the_last_provisioning_node_must_be_reported_as_last() {
        let mut d = initial(42);
        d.at_ms = 60_000;
        d.operation = 1;
        for _ in 0..2 {
            let n = add_node(&mut d, Group::GeneralLarge);
            n.phase = NodePhase::Provisioning;
            n.operation = 1;
            n.due_at = Some(50_000);
        }
        let e = Engine::from_state(Phase::ScalingUp, d, None).unwrap();
        let early = Event::NodeReady {
            node: 5,
            last: true,
        };
        assert_eq!(rejected(&e, early).await, "operation");
        let first = Event::NodeReady {
            node: 5,
            last: false,
        };
        e.machine.handle_event(first).await.unwrap();
        let late = Event::NodeReady {
            node: 6,
            last: false,
        };
        assert_eq!(rejected(&e, late).await, "operation");
        check(&e);
        assert_eq!(e.phase(), Phase::ScalingUp);
    }

    #[tokio::test]
    async fn a_stocked_out_group_fails_to_provision_and_frees_its_slot() {
        let mut e = Engine::new(42).unwrap();
        let revision = e.data().revision;
        let stockout = |enabled| Control::Stockout {
            group: Group::MemoryHeavy,
            enabled,
        };
        e.control(stockout(true)).await.unwrap();
        assert_eq!(e.data().revision, revision, "stock is not cluster state");
        let heavy = Control::MemoryHeavy {
            workload: API,
            enabled: true,
        };
        e.control(heavy).await.unwrap();
        let surge = Control::ReplicaSurge {
            workload: BATCH,
            enabled: true,
        };
        e.control(surge).await.unwrap();
        // One doomed and one healthy node share the operation.
        let outcome = apply(&mut e, up(Group::MemoryHeavy, 1)).await;
        assert_eq!(outcome.status, "applied");
        let outcome = apply(&mut e, up(Group::GeneralLarge, 1)).await;
        assert_eq!(outcome.status, "applied");
        e.advance(STOCKOUT_FAILURE_MS - 1).await.unwrap();
        assert_eq!(e.data().count(None, NodePhase::Provisioning), 2);
        e.advance(STOCKOUT_FAILURE_MS).await.unwrap();
        check(&e);
        let d = e.data();
        assert_eq!(e.phase(), Phase::ScalingUp);
        let failed = d.node(5).unwrap();
        assert_eq!((failed.phase, failed.failed), (NodePhase::Removed, true));
        assert_eq!(d.group(Group::MemoryHeavy).failures, 1);
        assert_eq!(d.group(Group::MemoryHeavy).last_failure_at, Some(10_000));
        assert_eq!(d.active(Group::MemoryHeavy), 0);
        assert_eq!(d.last_scale_up_finished_at, None);
        e.advance(40_000).await.unwrap();
        assert_eq!(e.phase(), Phase::Stable);
        assert_eq!(on_node(&e.data(), "large-6"), ["batch"]);
        assert_eq!(e.data().pending_count(), 2);
        let log = e.take_log();
        assert!(log[2].trigger.contains("mem-5 failed to provision"));
        assert!(log[2].trigger.contains("out of stock"));

        // A lone failure ends the operation; restocking lets the next request through.
        let outcome = apply(&mut e, up(Group::MemoryHeavy, 1)).await;
        assert_eq!(outcome.status, "applied");
        e.advance(50_000).await.unwrap();
        assert_eq!(e.phase(), Phase::Stable);
        e.control(stockout(false)).await.unwrap();
        let outcome = apply(&mut e, up(Group::MemoryHeavy, 1)).await;
        assert_eq!(outcome.status, "applied");
        e.advance(90_000).await.unwrap();
        check(&e);
        assert_eq!(e.data().pending_count(), 0);
        assert_eq!(e.data().group(Group::MemoryHeavy).failures, 2);
    }

    #[tokio::test]
    async fn no_change_errors_and_out_of_phase_actions_leave_the_cluster_alone() {
        let large = Group::GeneralLarge;
        let d = cluster(
            &[large, large, large, Group::GeneralSmall],
            &[(WEB, Some(0))],
        );
        let spare = d.nodes[2].clone();
        let mut e = Engine::from_state(Phase::Stable, d, None).unwrap();
        e.set_replicas(BATCH, 5).await.unwrap();
        e.set_replicas(API, 4).await.unwrap();
        assert!(e.data().pending_count() > 0);

        let before = snapshot(&e);
        let outcome = e.evaluation_error("timeout", "deadline exceeded");
        assert_eq!(outcome.await.unwrap().status, "evaluation_error");
        let invalid = act(&e.data(), up(large, 1));
        let outcome = e.apply(invalid, Some(1.5)).await.unwrap();
        assert_eq!(
            outcome.status, "evaluation_error",
            "an invalid confidence is an evaluation error, not a scale-up"
        );
        assert_eq!(apply(&mut e, Choice::NoChange).await.status, "unchanged");
        assert_eq!(before, snapshot(&e));

        assert_eq!(apply(&mut e, up(large, 1)).await.status, "applied");
        let before = snapshot(&e);
        assert_eq!(apply(&mut e, Choice::NoChange).await.status, "unchanged");
        let error = e.evaluation_error("timeout", "deadline exceeded");
        error.await.unwrap();
        let outcome = apply(&mut e, remove_node(&spare)).await;
        assert_eq!(outcome.code.as_deref(), Some("undefined_transition"));
        assert_eq!(before, snapshot(&e));
        assert_eq!(e.phase(), Phase::ScalingUp);

        e.advance(120_000).await.unwrap();
        e.set_replicas(BATCH, 0).await.unwrap();
        e.set_replicas(API, 0).await.unwrap();
        let d = e.data();
        let empty = d
            .nodes
            .iter()
            .find(|n| n.phase == NodePhase::Ready && n.used_cpu == 0);
        let outcome = apply(&mut e, remove_node(empty.unwrap())).await;
        assert_eq!(outcome.status, "applied");
        let before = snapshot(&e);
        assert_eq!(apply(&mut e, Choice::NoChange).await.status, "unchanged");
        let error = e.evaluation_error("timeout", "deadline exceeded");
        error.await.unwrap();
        let outcome = apply(&mut e, up(large, 1)).await;
        assert_eq!(outcome.code.as_deref(), Some("undefined_transition"));
        assert_eq!(before, snapshot(&e));
        assert_eq!(e.phase(), Phase::ScalingDown);
        let offered = legal_choices(Phase::ScalingDown, &e.data());
        assert_eq!(offered, [Choice::NoChange]);
    }

    #[test]
    fn each_invariant_rejects_the_state_it_guards() {
        let code = |phase: Phase, d: &Data| invariant(&phase, d).unwrap_err().code;
        let d = initial(42);
        let mut broken = d.clone();
        broken.nodes[0].used_cpu = broken.nodes[0].cpu + 1;
        assert_eq!(code(Phase::Stable, &broken), "overcommitted");
        let mut broken = d.clone();
        broken.nodes[0].used_memory_gib += 1;
        assert_eq!(code(Phase::Stable, &broken), "reservations");
        let mut broken = d.clone();
        broken.nodes[0].phase = NodePhase::Removed;
        assert_eq!(code(Phase::Stable, &broken), "pod_assignment");
        let mut broken = d.clone();
        broken.groups[0].max = 1;
        assert_eq!(code(Phase::Stable, &broken), "group_limits");
        let mut broken = d.clone();
        broken.groups[2].min = 1;
        assert_eq!(code(Phase::Stable, &broken), "group_limits");
        let mut broken = d.clone();
        broken.node_budget = 3;
        assert_eq!(code(Phase::Stable, &broken), "group_limits");
        assert_eq!(code(Phase::ScalingUp, &d), "phase_data");
        assert_eq!(code(Phase::ScalingDown, &d), "phase_data");
        let mut scaling = d.clone();
        add_node(&mut scaling, Group::GeneralLarge).phase = NodePhase::Provisioning;
        assert_eq!(code(Phase::Stable, &scaling), "phase_data");
        assert!(invariant(&Phase::ScalingUp, &scaling).is_ok());
        // An initial state that breaks an invariant never becomes a machine.
        assert!(Engine::from_state(Phase::ScalingDown, d, None).is_err());
    }

    #[tokio::test]
    async fn scaling_in_deletes_pending_replicas_first_and_keeps_reservations_exact() {
        let mut e = Engine::new(42).unwrap();
        let surge = |enabled| Control::ReplicaSurge {
            workload: WEB,
            enabled,
        };
        e.control(surge(true)).await.unwrap();
        check(&e);
        assert_eq!((e.data().pending_count(), e.data().available(WEB)), (8, 4));
        e.set_replicas(WEB, 2).await.unwrap();
        check(&e);
        // Six desired: every pending replica beyond the four running ones is deleted.
        assert_eq!(e.data().pending_count(), 2);
        assert_eq!(e.data().available(WEB), 4);
        e.control(surge(false)).await.unwrap();
        check(&e);
        let d = e.data();
        assert_eq!((d.pending_count(), d.available(WEB)), (0, 2));
        assert_eq!(d.requested().cpu, 2 * 2 + 2 * 3 + 4);
        let unknown = Event::Demand {
            workload: 9,
            replicas: 1,
        };
        assert_eq!(rejected(&e, unknown).await, "unknown_target");
        assert!(e.set_replicas(WEB, MAX_REPLICAS + 1).await.is_err());
    }

    #[tokio::test]
    async fn the_same_seed_reproduces_a_run_at_any_clock_granularity() {
        async fn run(seed: u64, step: u64) -> (serde_json::Value, serde_json::Value, Data) {
            let mut e = Engine::new(seed).unwrap();
            let mut at = 0;
            for (stage, until) in [10_000, 12_000, 70_000, 150_000, 200_000]
                .into_iter()
                .enumerate()
            {
                while at < until {
                    at = (at + step).min(until);
                    e.advance(at).await.unwrap();
                }
                match stage {
                    0 => {
                        let surge = Control::ReplicaSurge {
                            workload: WEB,
                            enabled: true,
                        };
                        e.control(surge).await.unwrap()
                    }
                    1 => {
                        apply(&mut e, up(Group::GeneralLarge, 2)).await;
                        apply(&mut e, up(Group::GeneralSmall, 2)).await;
                    }
                    2 => e.set_replicas(WEB, 1).await.unwrap(),
                    3 => {
                        let d = e.data();
                        let node = d.nodes.iter().rev().find(|n| n.phase == NodePhase::Ready);
                        apply(&mut e, remove_node(node.unwrap())).await;
                    }
                    _ => {}
                }
                check(&e);
            }
            let log = serde_json::to_value(e.take_log()).unwrap();
            (snapshot(&e), log, e.data())
        }
        let (coarse, coarse_log, d) = run(42, 1_000).await;
        for step in [50, 137, 200_000] {
            let (fine, fine_log, _) = run(42, step).await;
            assert_eq!(coarse, fine, "step {step}");
            assert_eq!(coarse_log, fine_log, "step {step}");
        }
        // Billing and waiting are exact integers of simulated time, so they agree too.
        assert_eq!(d.at_ms, 200_000);
        assert!(d.node_ms.iter().sum::<u64>() > 4 * 200_000);
        assert!(d.pending_pod_ms > 0 && d.cost_usd() > 0.);
        assert!(d.nodes.iter().any(|n| n.phase == NodePhase::Removed));
        let (other, ..) = run(7, 1_000).await;
        assert_ne!(coarse, other, "the seed draws provisioning times");
    }
}
