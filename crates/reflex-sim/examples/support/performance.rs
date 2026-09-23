// Unless explicitly stated otherwise all files in this repository are licensed under the Apache-2.0 License.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-present Datadog, Inc.

//! Observation-only feedback for the capacity study; no predicted execution times.
use reflex_sim::capacity::engine::{Data, JobPhase};
use serde_json::{json, Value};
use std::collections::{BTreeSet, VecDeque};

#[derive(Default)]
struct Sample {
    at_ms: u64,
    offered: u64,
    completed: u64,
    rejected: u64,
    waits_s: Vec<f64>,
    completions_s: Vec<f64>,
    node_seconds: u64,
    cpu_seconds: u64,
    memory_gib_seconds: u64,
}

#[derive(Default)]
pub struct Performance {
    samples: VecDeque<Sample>,
    terminal_seen: BTreeSet<u64>,
}
impl Performance {
    /// Called after each one-second tick, before applying the next control action.
    pub fn observe(&mut self, before: &Data, after: &Data) {
        assert_eq!(after.at_ms, before.at_ms + 1000);
        let mut sample = Sample {
            at_ms: after.at_ms,
            offered: after.offered - before.offered,
            node_seconds: before.active() as u64,
            cpu_seconds: before.nodes.iter().map(|n| n.used_cpu as u64).sum(),
            memory_gib_seconds: before.nodes.iter().map(|n| n.used_memory_gib as u64).sum(),
            ..Default::default()
        };
        for job in &after.jobs {
            if !matches!(job.phase, JobPhase::Completed | JobPhase::Rejected)
                || !self.terminal_seen.insert(job.arrival.id)
            {
                continue;
            }
            if job.phase == JobPhase::Completed {
                sample.completed += 1;
                sample
                    .waits_s
                    .push((job.started_at.unwrap() - job.arrived_at) as f64 / 1000.);
                // Observe completion now; never read actual_s or a running job's finish_at.
                sample
                    .completions_s
                    .push((after.at_ms - job.arrived_at) as f64 / 1000.);
            } else {
                sample.rejected += 1;
            }
        }
        self.samples.push_back(sample);
        self.samples
            .retain(|s| s.at_ms > after.at_ms.saturating_sub(120_000));
    }
    fn window(&self, from: u64, through: u64) -> Value {
        let samples: Vec<_> = self
            .samples
            .iter()
            .filter(|s| s.at_ms > from && s.at_ms <= through)
            .collect();
        let seconds = samples.len() as f64;
        let offered: u64 = samples.iter().map(|s| s.offered).sum();
        let completed: u64 = samples.iter().map(|s| s.completed).sum();
        let rejected: u64 = samples.iter().map(|s| s.rejected).sum();
        let nodes: u64 = samples.iter().map(|s| s.node_seconds).sum();
        let cpu: u64 = samples.iter().map(|s| s.cpu_seconds).sum();
        let mem: u64 = samples.iter().map(|s| s.memory_gib_seconds).sum();
        let waits: Vec<_> = samples
            .iter()
            .flat_map(|s| s.waits_s.iter().copied())
            .collect();
        let completions: Vec<_> = samples
            .iter()
            .flat_map(|s| s.completions_s.iter().copied())
            .collect();
        json!({
            "from_ms_exclusive":from,"through_ms":through,"observed_seconds":seconds,
            "offered":offered,"completed":completed,"rejected":rejected,
            "offered_per_s":ratio(offered,seconds),"completed_per_s":ratio(completed,seconds),
            "rejected_per_s":ratio(rejected,seconds),
            "rejected_fraction_of_terminal_outcomes":ratio(rejected,(completed+rejected) as f64),
            "completed_job_queue_wait_s":distribution(&waits),
            "completed_job_latency_s":distribution(&completions),
            "active_node_seconds":nodes,"mean_active_nodes":ratio(nodes,seconds),
            "cpu_reservation_utilization":ratio(cpu,(nodes*8) as f64),
            "memory_reservation_utilization":ratio(mem,(nodes*16) as f64)
        })
    }
    pub fn evidence(&self, data: &Data) -> Value {
        let now = data.at_ms;
        let actions: Vec<_> = data
            .changes
            .iter()
            .filter(|c| c.at_ms > now.saturating_sub(120_000) && c.at_ms <= now)
            .collect();
        json!({"recent_60s":self.window(now.saturating_sub(60_000),now),
            "previous_60s":self.window(now.saturating_sub(120_000),now.saturating_sub(60_000)),
            "applied_capacity_changes_120s":actions})
    }
}
fn ratio(n: u64, d: f64) -> Option<f64> {
    if d > 0. {
        Some(n as f64 / d)
    } else {
        None
    }
}
fn distribution(values: &[f64]) -> Value {
    let mean = if values.is_empty() {
        None
    } else {
        Some(values.iter().sum::<f64>() / values.len() as f64)
    };
    json!({"count":values.len(),"mean":mean,"p25":super::percentile(values,0.25),
        "p50":super::percentile(values,0.5),"p99":super::percentile(values,0.99)})
}

#[cfg(test)]
mod tests {
    use super::*;
    use reflex_sim::capacity::{
        engine::{Engine, Event, Settings},
        workload::{Arrival, Bucket},
    };
    #[tokio::test]
    async fn feedback_only_counts_observed_outcomes_and_uses_disjoint_windows() {
        let mut engine = Engine::new(Settings::default()).unwrap();
        let mut perf = Performance::default();
        assert!(perf.evidence(&engine.data())["recent_60s"]["completed_per_s"].is_null());
        for t in 1..=122 {
            let before = engine.data();
            let jobs = if t == 1 {
                vec![
                    Arrival {
                        id: 1,
                        client: 0,
                        cpu: 1,
                        memory_gib: 1,
                        estimated_s: 6,
                        actual_s: 2,
                    },
                    Arrival {
                        id: 2,
                        client: 0,
                        cpu: 99,
                        memory_gib: 1,
                        estimated_s: 6,
                        actual_s: 999,
                    },
                ]
            } else {
                vec![]
            };
            engine
                .event(Event::Tick(Bucket {
                    at_ms: t * 1000,
                    values: [0.; 3],
                    jobs,
                }))
                .await
                .unwrap();
            let after = engine.data();
            perf.observe(&before, &after);
            let evidence = perf.evidence(&after);
            let recent = &evidence["recent_60s"];
            if t == 1 {
                assert_eq!(recent["offered"], 2);
                assert_eq!(recent["completed"], 0);
                assert_eq!(recent["rejected"], 1);
                assert!(recent["completed_job_latency_s"]["mean"].is_null());
                let mut hidden = after.clone();
                hidden.jobs[0].arrival.actual_s = 999;
                hidden.jobs[0].finish_at = Some(999_000);
                let mut other = Performance::default();
                other.observe(&before, &hidden);
                assert_eq!(evidence, other.evidence(&hidden));
            }
            if t == 3 {
                assert_eq!(recent["completed"], 1);
                assert_eq!(recent["completed_job_latency_s"]["mean"], 2.0);
            }
            if t == 61 {
                assert_eq!(recent["rejected"], 0);
                assert_eq!(evidence["previous_60s"]["rejected"], 1);
                assert_eq!(recent["completed"], 1);
                assert_eq!(recent["observed_seconds"], 60.0);
            }
            if t == 63 {
                assert_eq!(recent["completed"], 0);
                assert_eq!(evidence["previous_60s"]["completed"], 1);
            }
        }
        assert_eq!(perf.samples.len(), 120);
    }
}
