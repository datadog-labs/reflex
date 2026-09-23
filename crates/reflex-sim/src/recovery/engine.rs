// Unless explicitly stated otherwise all files in this repository are licensed under the Apache-2.0 License.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-present Datadog, Inc.

//! A deterministic, 50 ms event lattice. Physical faults never enter judge evidence.
use super::{
    telemetry::{MetricEvent, Telemetry},
    Policy,
};
use crate::Error;
use opentelemetry::metrics::Meter;
use reflex::{
    state_machine, ExecutionOutcome, InMemory, Judgment, Rejection, StateMachineExecutor,
};
use serde::{Deserialize, Serialize};
use std::time::Instant;
pub const HORIZON: u64 = 180_000;
pub const STEP: u64 = 50;
pub const SNAPSHOT_MB: f64 = 100.;
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Lifecycle {
    Empty,
    Rebuilding,
    Checking,
    Ready,
    Unavailable,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Faults {
    pub crashed: bool,
    pub client_partition: bool,
    pub transfer_partition: bool,
    pub slowdown: f64,
    pub error_rate: f64,
    pub stale: bool,
}
impl Default for Faults {
    fn default() -> Self {
        Self {
            crashed: false,
            client_partition: false,
            transfer_partition: false,
            slowdown: 1.,
            error_rate: 0.,
            stale: false,
        }
    }
}
impl Faults {
    pub fn valid(&self) -> bool {
        self.slowdown.is_finite()
            && (1. ..=10.).contains(&self.slowdown)
            && self.error_rate.is_finite()
            && (0. ..=1.).contains(&self.error_rate)
    }
}
#[derive(Clone, Debug, Serialize)]
pub struct Replica {
    pub id: usize,
    pub name: String,
    pub phase: Lifecycle,
    pub serving: bool,
    pub version: u64,
    pub reachable: bool,
    pub transfer_reachable: bool,
    pub heartbeat_at: u64,
    pub checking_since: u64,
    pub faults: Faults,
    pub completed: u64,
    pub failures: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClientConfig {
    pub rate: f64,
    pub cost_ms: u64,
    pub essential_pct: u8,
    pub enabled: bool,
}
impl ClientConfig {
    pub fn valid(&self) -> bool {
        self.rate.is_finite()
            && (0. ..=40.).contains(&self.rate)
            && (50..=1000).contains(&self.cost_ms)
            && self.essential_pct <= 100
    }
}
#[derive(Clone, Debug, Serialize)]
pub struct Client {
    pub id: u64,
    pub config: ClientConfig,
    pub sequence: u64,
    pub next_at: Option<u64>,
}
impl Client {
    pub fn schedule(&mut self, now: u64) {
        self.next_at = if self.config.enabled && self.config.rate > 0. {
            Some(now + (1000. / self.config.rate).round() as u64)
        } else {
            None
        };
    }
}
#[derive(Clone, Debug, Serialize)]
pub struct Request {
    pub id: u64,
    pub client: u64,
    pub essential: bool,
    pub node: usize,
    pub attempt: u8,
    pub arrived_at: u64,
    pub attempt_at: u64,
    pub cost_ms: f64,
    pub remaining: f64,
    #[serde(skip)]
    pub(crate) received_at: Option<u64>,
    #[serde(skip)]
    pub(crate) started_at: Option<u64>,
}
#[derive(Clone, Debug, Serialize)]
pub struct Outcome {
    pub at_ms: u64,
    pub node: Option<usize>,
    pub success: bool,
    pub essential: bool,
    pub rejected: bool,
    pub latency_ms: u64,
    pub retried: bool,
}
#[derive(Clone, Debug, Serialize)]
pub struct Motion {
    pub id: u64,
    pub at_ms: u64,
    pub client: u64,
    pub node: Option<usize>,
    pub retry: bool,
    pub rejected: bool,
    pub cost_ms: f64,
}
#[derive(Clone, Debug, Serialize)]
pub struct Recovery {
    pub id: u64,
    pub source: usize,
    pub target: usize,
    pub phase: String,
    pub progress_mb: f64,
    pub rate: f64,
    pub started_at: u64,
    pub last_progress: u64,
    pub verify_at: Option<u64>,
    pub finished_at: Option<u64>,
    pub reason: Option<String>,
}
impl Recovery {
    pub fn active(&self) -> bool {
        matches!(self.phase.as_str(), "rebuilding" | "verifying")
    }
}
#[derive(Clone, Debug, Default, Serialize)]
pub struct Totals {
    pub arrivals: u64,
    pub essential_arrivals: u64,
    pub succeeded: u64,
    pub essential_succeeded: u64,
    pub failed: u64,
    pub rejected: u64,
    pub retries: u64,
    pub rebuilds: u64,
    pub recovery_ms: u64,
}
#[derive(Clone, Debug, Serialize)]
pub struct ActionRecord {
    pub at_ms: u64,
    pub action: Action,
    pub description: String,
}
#[derive(Clone, Debug, Serialize)]
pub struct Data {
    pub at_ms: u64,
    pub revision: u64,
    pub seed: u64,
    pub replicas: Vec<Replica>,
    pub clients: Vec<Client>,
    pub essential_only: bool,
    pub retries_enabled: bool,
    pub retry_credit: f64,
    pub bandwidth_limit: f64,
    pub recovery: Option<Recovery>,
    pub intervention: bool,
    pub last_action: u64,
    pub recent_actions: Vec<ActionRecord>,
    pub requests: Vec<Request>,
    pub outcomes: Vec<Outcome>,
    pub motions: Vec<Motion>,
    pub totals: Totals,
    pub next_request: u64,
    pub next_motion: u64,
    pub next_recovery: u64,
    pub route_cursor: usize,
    #[serde(skip)]
    pub(super) metric_events: Vec<MetricEvent>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Action {
    KeepCurrentPlan,
    RemoveFromServing { replica: usize },
    AddToServing { replica: usize },
    SetRetryBudget { enabled: bool },
    StartRebuild { source: usize, target: usize },
    SetRebuildRate { high: bool },
    CancelRebuild,
    SetServingMode { essential_only: bool },
    RequestIntervention,
}
impl Action {
    pub fn label(self) -> String {
        let name = |i: usize| char::from(b'A' + i as u8);
        match self {
            Self::KeepCurrentPlan => "Keep current plan".into(),
            Self::RemoveFromServing { replica } => {
                format!("Exclude {} from serving", name(replica))
            }
            Self::AddToServing { replica } => format!("Route reads to {}", name(replica)),
            Self::SetRetryBudget { enabled } => format!(
                "{} bounded retries",
                if enabled { "Enable" } else { "Disable" }
            ),
            Self::StartRebuild { source, target } => {
                format!("Rebuild {} from {}", name(target), name(source))
            }
            Self::SetRebuildRate { high } => {
                format!("Set rebuild rate {}", if high { "high" } else { "low" })
            }
            Self::CancelRebuild => "Cancel active rebuild".into(),
            Self::SetServingMode { essential_only } => if essential_only {
                "Serve essential reads only"
            } else {
                "Restore normal service"
            }
            .into(),
            Self::RequestIntervention => "Request operator intervention".into(),
        }
    }
}
#[derive(Clone)]
pub struct Proposal {
    pub action: Action,
    pub observed_at: u64,
    pub revision: u64,
}
#[derive(Clone, Debug)]
pub enum Event {
    Tick,
    Fault { replica: usize, faults: Faults },
    Client { id: u64, config: ClientConfig },
    AddClient,
    RemoveClient(u64),
    Bandwidth(f64),
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Operating,
}
fn reject(code: &str, message: &str) -> Rejection {
    Rejection::new(code, message)
}
pub fn legal(d: &Data, a: Action) -> Result<(), Rejection> {
    let eligible = |i: usize| {
        d.replicas
            .get(i)
            .is_some_and(|r| r.phase == Lifecycle::Ready && r.version == 1 && r.reachable)
    };
    let active = d.recovery.as_ref().filter(|r| r.active());
    match a {
        Action::KeepCurrentPlan => {}
        Action::RemoveFromServing { replica } => {
            if !d.replicas.get(replica).is_some_and(|r| r.serving)
                || !d
                    .replicas
                    .iter()
                    .enumerate()
                    .any(|(i, r)| i != replica && r.serving && eligible(i))
            {
                return Err(reject(
                    "serving_pool",
                    "Retain at least one observed ready serving replica",
                ));
            }
        }
        Action::AddToServing { replica } => {
            if !eligible(replica) || d.replicas[replica].serving {
                return Err(reject(
                    "readiness",
                    "Replica must be ready, fresh, reachable and excluded",
                ));
            }
        }
        Action::SetRetryBudget { enabled } => {
            if enabled == d.retries_enabled {
                return Err(reject("unchanged", "Retry budget already selected"));
            }
        }
        Action::StartRebuild { source, target } => {
            if source == target
                || !eligible(source)
                || !d.replicas[source].transfer_reachable
                || !d.replicas.get(target).is_some_and(|r| {
                    matches!(
                        r.phase,
                        Lifecycle::Empty | Lifecycle::Unavailable | Lifecycle::Checking
                    ) && r.transfer_reachable
                })
                || active.is_some()
                || d.bandwidth_limit < 4.
            {
                return Err(reject("rebuild_eligibility","Need a fresh source, reachable replacement, 4 MB/s budget and no active rebuild"));
            }
            if d.replicas
                .iter()
                .filter(|r| r.phase == Lifecycle::Ready)
                .count()
                >= 3
            {
                return Err(reject(
                    "redundancy",
                    "Desired redundancy of three is already restored",
                ));
            }
        }
        Action::SetRebuildRate { high } => {
            let rate = if high { 12. } else { 4. };
            if active.is_none_or(|r| r.phase != "rebuilding" || r.rate == rate)
                || rate > d.bandwidth_limit
            {
                return Err(reject(
                    "bandwidth",
                    "Rate must change an active transfer within the configured budget",
                ));
            }
        }
        Action::CancelRebuild => {
            if active.is_none() {
                return Err(reject("operation", "No active rebuild"));
            }
        }
        Action::SetServingMode { essential_only } => {
            if d.essential_only == essential_only {
                return Err(reject("unchanged", "Serving mode already selected"));
            }
        }
        Action::RequestIntervention => {
            if d.intervention {
                return Err(reject("deduplicate", "Intervention already requested"));
            }
        }
    }
    Ok(())
}
pub fn actions(d: &Data) -> Vec<Action> {
    let mut all = vec![
        Action::KeepCurrentPlan,
        Action::SetRetryBudget { enabled: true },
        Action::SetRetryBudget { enabled: false },
        Action::SetRebuildRate { high: false },
        Action::SetRebuildRate { high: true },
        Action::CancelRebuild,
        Action::SetServingMode {
            essential_only: true,
        },
        Action::SetServingMode {
            essential_only: false,
        },
        Action::RequestIntervention,
    ];
    for replica in 0..4 {
        all.push(Action::RemoveFromServing { replica });
        all.push(Action::AddToServing { replica });
        for source in 0..4 {
            all.push(Action::StartRebuild {
                source,
                target: replica,
            });
        }
    }
    all.into_iter().filter(|a| legal(d, *a).is_ok()).collect()
}
fn guard(d: &Data, p: &Proposal, _: Instant) -> Result<(), Rejection> {
    if p.observed_at > d.at_ms || d.at_ms - p.observed_at > 5000 {
        return Err(reject(
            "stale",
            "Evidence must be no more than five simulated seconds old",
        ));
    }
    if p.revision != d.revision {
        return Err(reject(
            "changed",
            "Configuration or lifecycle changed since evaluation",
        ));
    }
    if p.action != Action::KeepCurrentPlan && d.last_action > 0 && d.at_ms < d.last_action + 1000 {
        return Err(reject(
            "cooldown",
            "Wait one simulated second between interventions",
        ));
    }
    legal(d, p.action)
}
fn apply(d: &mut Data, p: &Proposal, _: Instant) -> Result<(), Rejection> {
    d.metric_events.clear();
    match p.action {
        Action::KeepCurrentPlan => return Ok(()),
        Action::RemoveFromServing { replica } => d.replicas[replica].serving = false,
        Action::AddToServing { replica } => d.replicas[replica].serving = true,
        Action::SetRetryBudget { enabled } => d.retries_enabled = enabled,
        Action::SetServingMode { essential_only } => d.essential_only = essential_only,
        Action::RequestIntervention => d.intervention = true,
        Action::StartRebuild { source, target } => {
            d.next_recovery += 1;
            d.replicas[target].serving = false;
            d.replicas[target].phase = Lifecycle::Rebuilding;
            d.replicas[target].version = 0;
            d.recovery = Some(Recovery {
                id: d.next_recovery,
                source,
                target,
                phase: "rebuilding".into(),
                progress_mb: 0.,
                rate: 4.,
                started_at: d.at_ms,
                last_progress: d.at_ms,
                verify_at: None,
                finished_at: None,
                reason: None,
            });
        }
        Action::SetRebuildRate { high } => {
            d.recovery.as_mut().unwrap().rate = if high { 12. } else { 4. }
        }
        Action::CancelRebuild => {
            let r = d.recovery.as_mut().unwrap();
            r.phase = "cancelled".into();
            r.finished_at = Some(d.at_ms);
            r.reason = Some("Cancelled by policy".into());
            d.replicas[r.target].phase = Lifecycle::Unavailable;
        }
    }
    d.recent_actions.push(ActionRecord {
        at_ms: d.at_ms,
        action: p.action,
        description: p.action.label(),
    });
    if d.recent_actions.len() > 8 {
        d.recent_actions.remove(0);
    }
    d.last_action = d.at_ms;
    d.revision += 1;
    Ok(())
}
pub fn invariant(_: &Phase, d: &Data) -> Result<(), Rejection> {
    if d.replicas.len() != 4
        || d.clients.is_empty()
        || d.clients.len() > 8
        || !d.bandwidth_limit.is_finite()
        || !(0. ..=16.).contains(&d.bandwidth_limit)
        || !(0. ..=2.00001).contains(&d.retry_credit)
    {
        return Err(reject(
            "bounds",
            "Resource budgets and topology must remain bounded",
        ));
    }
    if d.clients.iter().any(|c| !c.config.valid())
        || d.replicas
            .iter()
            .any(|r| !r.faults.valid() || (r.phase == Lifecycle::Ready && r.version != 1))
    {
        return Err(reject(
            "configuration",
            "Ready replicas must have a verified snapshot; configurations must be valid",
        ));
    }
    if let Some(r) = &d.recovery {
        if r.active()
            && (r.source == r.target
                || r.source >= 4
                || r.target >= 4
                || r.rate > d.bandwidth_limit
                || r.rate < 0.
                || !r.progress_mb.is_finite()
                || !(0. ..=SNAPSHOT_MB).contains(&r.progress_mb)
                || d.replicas[r.target].serving)
        {
            return Err(reject("rebuild","Active rebuild must have distinct endpoints, a bounded rate and an excluded target"));
        }
    }
    for n in 0..4 {
        if d.requests.iter().filter(|r| r.node == n).count() > 32 {
            return Err(reject("queue_bound", "Per-replica queue limit exceeded"));
        }
    }
    if d.requests
        .iter()
        .any(|r| r.node >= 4 || r.attempt > 1 || !r.remaining.is_finite())
    {
        return Err(reject(
            "request",
            "Invalid request assignment or retry count",
        ));
    }
    if d.totals.arrivals
        != d.totals.succeeded + d.totals.failed + d.totals.rejected + d.requests.len() as u64
    {
        return Err(reject(
            "conservation",
            "Every original request must be in flight or have one terminal outcome",
        ));
    }
    Ok(())
}
fn draw(seed: u64, id: u64, salt: u64) -> f64 {
    let mut v = seed ^ id.wrapping_mul(0x9e3779b97f4a7c15) ^ salt;
    v = (v ^ (v >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    v = (v ^ (v >> 27)).wrapping_mul(0x94d049bb133111eb);
    ((v ^ (v >> 31)) >> 11) as f64 / (1u64 << 53) as f64
}
fn motion(d: &mut Data, r: &Request, node: Option<usize>, rejected: bool) {
    d.next_motion += 1;
    d.motions.push(Motion {
        id: d.next_motion,
        at_ms: d.at_ms,
        client: r.client,
        node,
        retry: r.attempt > 0,
        rejected,
        cost_ms: r.cost_ms,
    });
}
fn finish(d: &mut Data, r: &Request, success: bool, rejected: bool, reason: &'static str) {
    d.metric_events.push(MetricEvent::Client {
        request: r.clone(),
        success,
        rejected,
        reason,
    });
    if success {
        d.totals.succeeded += 1;
        if r.essential {
            d.totals.essential_succeeded += 1;
        }
    } else if rejected {
        d.totals.rejected += 1;
    } else {
        d.totals.failed += 1;
    }
    d.outcomes.push(Outcome {
        at_ms: d.at_ms,
        node: if rejected { None } else { Some(r.node) },
        success,
        essential: r.essential,
        rejected,
        latency_ms: d.at_ms - r.arrived_at,
        retried: r.attempt > 0,
    });
}
fn route(d: &mut Data, mut r: Request, exclude: Option<usize>) {
    let targets: Vec<_> = d
        .replicas
        .iter()
        .filter(|n| n.serving && Some(n.id) != exclude)
        .map(|n| n.id)
        .collect();
    if targets.is_empty() {
        motion(d, &r, None, true);
        finish(d, &r, false, true, "no_serving_replica");
        return;
    }
    r.node = targets[d.route_cursor % targets.len()];
    d.route_cursor += 1;
    let replica = &d.replicas[r.node];
    if !replica.faults.crashed && !replica.faults.client_partition {
        r.received_at = Some(d.at_ms);
    }
    if d.requests.iter().filter(|q| q.node == r.node).count() >= 32 {
        motion(d, &r, Some(r.node), true);
        d.metric_events.push(MetricEvent::Attempt {
            request: r.clone(),
            outcome: "queue_full",
        });
        finish(d, &r, false, true, "queue_full");
        return;
    }
    motion(d, &r, Some(r.node), false);
    d.requests.push(r);
}
fn tick(d: &mut Data) {
    d.at_ms += STEP;
    // Health probes run every 500 ms, not at fault injection time.
    if d.at_ms.is_multiple_of(500) {
        for r in &mut d.replicas {
            let before = (r.phase, r.reachable, r.transfer_reachable, r.version);
            r.reachable = !r.faults.crashed && !r.faults.client_partition;
            r.transfer_reachable = !r.faults.crashed && !r.faults.transfer_partition;
            if r.reachable {
                r.heartbeat_at = d.at_ms;
            }
            if r.phase != Lifecycle::Empty && r.phase != Lifecycle::Rebuilding {
                if !r.reachable {
                    r.phase = Lifecycle::Unavailable;
                } else if r.faults.stale {
                    r.version = 0;
                    r.phase = Lifecycle::Checking;
                } else if r.phase == Lifecycle::Unavailable {
                    r.phase = Lifecycle::Checking;
                    r.checking_since = d.at_ms;
                } else if r.phase == Lifecycle::Checking
                    && r.version == 1
                    && d.at_ms >= r.checking_since + 1000
                {
                    r.phase = Lifecycle::Ready;
                }
            }
            if before != (r.phase, r.reachable, r.transfer_reachable, r.version) {
                d.revision += 1;
            }
        }
    }
    if let Some(r) = &mut d.recovery {
        if r.active() {
            let src = &d.replicas[r.source];
            let dst = &d.replicas[r.target];
            let connected = !src.faults.crashed
                && !src.faults.transfer_partition
                && !dst.faults.crashed
                && !dst.faults.transfer_partition
                && src.version == 1
                && !src.faults.stale;
            if connected && r.phase == "rebuilding" && r.rate > 0. {
                r.progress_mb = (r.progress_mb + r.rate * STEP as f64 / 1000.).min(SNAPSHOT_MB);
                r.last_progress = d.at_ms;
                if r.progress_mb >= SNAPSHOT_MB {
                    r.phase = "verifying".into();
                    r.verify_at = Some(d.at_ms + 1000);
                    d.replicas[r.target].phase = Lifecycle::Checking;
                    d.revision += 1;
                }
            } else if connected && r.phase == "verifying" && d.at_ms >= r.verify_at.unwrap() {
                r.phase = "completed".into();
                r.finished_at = Some(d.at_ms);
                let dst = &mut d.replicas[r.target];
                dst.version = 1;
                dst.faults.stale = false;
                dst.phase = if dst.reachable {
                    Lifecycle::Ready
                } else {
                    Lifecycle::Unavailable
                };
                dst.checking_since = d.at_ms;
                d.totals.rebuilds += 1;
                d.totals.recovery_ms += d.at_ms - r.started_at;
                d.revision += 1;
            }
            if r.active() && d.at_ms - r.last_progress >= 3000 && r.phase != "verifying" {
                r.phase = "failed".into();
                r.finished_at = Some(d.at_ms);
                r.reason = Some("No transfer progress for three seconds".into());
                d.replicas[r.target].phase = Lifecycle::Unavailable;
                d.revision += 1;
            }
            if r.phase == "verifying" && !connected && d.at_ms >= r.verify_at.unwrap() + 2000 {
                r.phase = "failed".into();
                r.finished_at = Some(d.at_ms);
                r.reason = Some("Verification could not reach the snapshot source".into());
                d.replicas[r.target].phase = Lifecycle::Unavailable;
                d.revision += 1;
            }
        }
    }
    let old = std::mem::take(&mut d.requests);
    let mut positions = [0usize; 4];
    let mut loads = [0usize; 4];
    for r in &old {
        loads[r.node] += 1;
    }
    let mut retry = vec![];
    for mut r in old {
        let n = &d.replicas[r.node];
        positions[r.node] += 1;
        let penalty = d
            .recovery
            .as_ref()
            .filter(|x| x.phase == "rebuilding" && (x.source == r.node || x.target == r.node))
            .map_or(0., |x| (x.rate / 16. * 0.7).min(0.7));
        let works = !n.faults.crashed
            && !n.faults.client_partition
            && n.phase == Lifecycle::Ready
            && !n.faults.stale;
        if r.received_at.is_none() && !n.faults.crashed && !n.faults.client_partition {
            r.received_at = Some(d.at_ms);
        }
        if r.received_at.is_some() && r.started_at.is_none() && positions[r.node] <= 4 {
            r.started_at = Some(d.at_ms);
            d.metric_events.push(MetricEvent::Wait(r.clone()));
        }
        if works && positions[r.node] <= 4 {
            r.remaining -= STEP as f64 / n.faults.slowdown * (1. - penalty);
        }
        let done = r.remaining <= 0.;
        let timeout = d.at_ms - r.attempt_at >= 1500;
        if done || timeout {
            let chance = (n.faults.error_rate
                + (loads[r.node].saturating_sub(8) as f64 / 32.) * 0.5)
                .min(1.);
            let ok = done && draw(d.seed, r.id, 93 + r.attempt as u64) > chance;
            let outcome = if ok {
                "success"
            } else if done {
                "server_error"
            } else {
                "timeout"
            };
            d.metric_events.push(MetricEvent::Attempt {
                request: r.clone(),
                outcome,
            });
            if ok {
                d.replicas[r.node].completed += 1;
                finish(d, &r, true, false, "success");
            } else {
                d.replicas[r.node].failures += 1;
                let suppression = if r.attempt > 0 {
                    Some("attempt_limit")
                } else if !d.retries_enabled {
                    Some("disabled")
                } else if d.retry_credit + 1e-9 < 1. {
                    Some("budget_exhausted")
                } else if !d.replicas.iter().any(|n| n.serving && n.id != r.node) {
                    Some("no_alternative")
                } else {
                    None
                };
                d.metric_events.push(MetricEvent::Retry {
                    request: r.clone(),
                    suppression,
                });
                if suppression.is_none() {
                    d.retry_credit = (d.retry_credit - 1.).max(0.);
                    d.totals.retries += 1;
                    r.attempt = 1;
                    r.attempt_at = d.at_ms;
                    r.remaining = r.cost_ms;
                    r.received_at = None;
                    r.started_at = None;
                    retry.push(r);
                } else {
                    finish(d, &r, false, false, outcome);
                }
            }
        } else {
            d.requests.push(r);
        }
    }
    for r in retry {
        let previous = r.node;
        route(d, r, Some(previous));
    }
    for i in 0..d.clients.len() {
        while d.clients[i].next_at.is_some_and(|at| at <= d.at_ms) {
            let c = &mut d.clients[i];
            c.sequence += 1;
            let essential =
                draw(d.seed, c.sequence, c.id * 137 + 11) * 100. < c.config.essential_pct as f64;
            d.next_request += 1;
            let r = Request {
                id: d.next_request,
                client: c.id,
                essential,
                node: 0,
                attempt: 0,
                arrived_at: d.at_ms,
                attempt_at: d.at_ms,
                cost_ms: c.config.cost_ms as f64,
                remaining: c.config.cost_ms as f64,
                received_at: None,
                started_at: None,
            };
            c.schedule(c.next_at.expect("scheduled arrival"));
            d.totals.arrivals += 1;
            if essential {
                d.totals.essential_arrivals += 1;
            }
            d.retry_credit = (d.retry_credit + 0.1).min(2.);
            if d.essential_only && !essential {
                motion(d, &r, None, true);
                finish(d, &r, false, true, "essential_only");
            } else {
                route(d, r, None);
            }
        }
    }
    d.outcomes.retain(|o| o.at_ms + 10_000 >= d.at_ms);
    d.motions.retain(|o| o.at_ms + 2000 >= d.at_ms);
}
fn event(d: &mut Data, e: &Event, _: Instant) -> Result<(), Rejection> {
    d.metric_events.clear();
    match e {
        Event::Tick => tick(d),
        Event::Fault { replica, faults } => {
            if !faults.valid() {
                return Err(reject("fault", "Invalid fault limits"));
            }
            let r = d
                .replicas
                .get_mut(*replica)
                .ok_or_else(|| reject("replica", "Unknown replica"))?;
            r.faults = faults.clone();
            d.revision += 1;
        }
        Event::Client { id, config } => {
            if !config.valid() {
                return Err(reject(
                    "client",
                    "Client limits: 0–40 requests/s, 50–1000 ms cost, 0–100% essential",
                ));
            }
            let c = d
                .clients
                .iter_mut()
                .find(|c| c.id == *id)
                .ok_or_else(|| reject("client", "Unknown client"))?;
            c.config = config.clone();
            c.schedule(d.at_ms);
            d.revision += 1;
        }
        Event::AddClient => {
            if d.clients.len() >= 8 {
                return Err(reject("clients", "At most eight clients"));
            }
            let id = d.clients.iter().map(|c| c.id).max().unwrap() + 1;
            let mut c = Client {
                id,
                config: ClientConfig {
                    rate: 3.,
                    cost_ms: 150,
                    essential_pct: 50,
                    enabled: true,
                },
                sequence: 0,
                next_at: None,
            };
            c.schedule(d.at_ms);
            d.clients.push(c);
            d.revision += 1;
        }
        Event::RemoveClient(id) => {
            if d.clients.len() <= 1 || !d.clients.iter().any(|c| c.id == *id) {
                return Err(reject("client", "Keep at least one known client"));
            }
            d.clients.retain(|c| c.id != *id);
            d.revision += 1;
        }
        Event::Bandwidth(limit) => {
            if !limit.is_finite() || !(0. ..=16.).contains(limit) {
                return Err(reject("bandwidth", "Bandwidth must be 0–16 MB/s"));
            }
            d.bandwidth_limit = *limit;
            if let Some(r) = &mut d.recovery {
                r.rate = r.rate.min(*limit);
            }
            d.revision += 1;
        }
    }
    Ok(())
}
pub struct Engine {
    machine: StateMachineExecutor<Phase, Data, Proposal, Event>,
    telemetry: Telemetry,
}
impl Engine {
    pub fn new(seed: u64) -> Result<Self, Error> {
        Self::with_meter(
            seed,
            Policy::Fixed,
            opentelemetry::global::meter("recovery"),
        )
    }
    pub fn with_meter(seed: u64, policy: Policy, meter: Meter) -> Result<Self, Error> {
        Self::with_run(seed, policy, meter, None)
    }
    pub(super) fn with_run(
        seed: u64,
        policy: Policy,
        meter: Meter,
        run: Option<String>,
    ) -> Result<Self, Error> {
        let definition = state_machine! {phase:Phase,data:Data,action:Proposal,event:Event,invariants:[invariant],transitions:[Phase::Operating+action(_)=>Phase::Operating{guard:guard,update:apply},Phase::Operating+event(_)=>Phase::Operating{update:event},Phase::Operating+evaluation_error(_)=>unchanged{}]};
        let replicas = (0..4)
            .map(|id| Replica {
                id,
                name: format!("Replica {}", char::from(b'A' + id as u8)),
                phase: if id == 3 {
                    Lifecycle::Empty
                } else {
                    Lifecycle::Ready
                },
                serving: id < 3,
                version: if id == 3 { 0 } else { 1 },
                reachable: true,
                transfer_reachable: true,
                heartbeat_at: 0,
                checking_since: 0,
                faults: Faults::default(),
                completed: 0,
                failures: 0,
            })
            .collect();
        let clients = (0..3)
            .map(|id| {
                let mut c = Client {
                    id,
                    config: ClientConfig {
                        rate: 8.,
                        cost_ms: 150,
                        essential_pct: if id == 0 { 100 } else { 40 },
                        enabled: true,
                    },
                    sequence: 0,
                    next_at: None,
                };
                c.schedule(0);
                c
            })
            .collect();
        let data = Data {
            at_ms: 0,
            revision: 0,
            seed,
            replicas,
            clients,
            essential_only: false,
            retries_enabled: false,
            retry_credit: 0.,
            bandwidth_limit: 12.,
            recovery: None,
            intervention: false,
            last_action: 0,
            recent_actions: vec![],
            requests: vec![],
            outcomes: vec![],
            motions: vec![],
            totals: Totals::default(),
            next_request: 0,
            next_motion: 0,
            next_recovery: 0,
            route_cursor: 0,
            metric_events: vec![],
        };
        let mut telemetry = Telemetry::new(meter, policy, run);
        telemetry.sync(&data);
        Ok(Self {
            telemetry,
            machine: StateMachineExecutor::builder(definition)
                .name("retry_recovery")
                .store(InMemory::new(Phase::Operating, data))
                .build()
                .map_err(|e| Error::Policy(e.to_string()))?,
        })
    }
    pub fn data(&self) -> Data {
        self.machine.state().expect("in-memory state").1
    }
    pub async fn event(&mut self, e: Event) -> Result<(), Error> {
        match self
            .machine
            .handle_event(e.clone())
            .await
            .map_err(|e| Error::Policy(e.to_string()))?
        {
            ExecutionOutcome::Applied(_) => {
                self.telemetry.sync(&self.data());
                if !matches!(e, Event::Tick) {
                    tracing::info!(target: "reflex_sim::recovery", event = ?e, "Recovery control applied");
                }
                Ok(())
            }
            ExecutionOutcome::Rejected { reason, .. } => Err(Error::Invalid(reason.message)),
        }
    }
    pub async fn apply(
        &mut self,
        p: Proposal,
        confidence: Option<f64>,
    ) -> Result<(bool, String), Error> {
        Ok(
            match self
                .machine
                .execute(
                    Judgment {
                        action: p.clone(),
                        confidence,
                    }
                    .try_into(),
                )
                .await
                .map_err(|e| Error::Policy(e.to_string()))?
            {
                ExecutionOutcome::Applied(r) if r.evaluation_error.is_some() => {
                    (false, "Invalid judgment; existing plan retained".into())
                }
                ExecutionOutcome::Applied(_) => {
                    self.telemetry.sync(&self.data());
                    tracing::info!(target: "reflex_sim::recovery", action = ?p.action, "Recovery action applied");
                    (
                        true,
                        "Freshness, lifecycle, serving-pool and resource guards passed".into(),
                    )
                }
                ExecutionOutcome::Rejected { reason, .. } => {
                    (false, format!("{}: {}", reason.code, reason.message))
                }
            },
        )
    }
    pub async fn evaluation_error(&mut self, message: &str) -> Result<(), Error> {
        self.machine
            .execute(Err(reflex::EvaluationError::Judge(
                reflex::JudgeError::new("recovery_inference", message),
            )))
            .await
            .map_err(|e| Error::Policy(e.to_string()))?;
        Ok(())
    }
}
