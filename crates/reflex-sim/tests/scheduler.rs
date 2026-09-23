// Unless explicitly stated otherwise all files in this repository are licensed under the Apache-2.0 License.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-present Datadog, Inc.

use reflex_sim::{
    playground::inference::JevSettings,
    scheduler::{
        engine::{invariant, Choice, Engine, Job, JobPhase, Phase, Placement},
        judge::{self, Evaluation, Evaluator, Evidence, Inference},
        ClientConfig, Command, Policy, Session,
    },
};
use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};
fn job(id: u64, cpu: u32, memory: u32) -> Job {
    Job {
        id,
        client: 0,
        cpu,
        memory_gib: memory,
        duration_ms: 1000,
        actual_duration_ms: 1000,
        arrived_at: 0,
        phase: JobPhase::Queued,
        node: None,
        started_at: None,
        completed_at: None,
        finish_at: None,
        reason: None,
    }
}
#[tokio::test]
async fn placements_are_atomic_and_guard_cpu_memory_fifo_and_freshness() {
    let mut e = Engine::new().unwrap();
    e.arrival(job(1, 3, 6)).await.unwrap();
    e.arrival(job(2, 2, 3)).await.unwrap();
    assert!(
        !e.apply(
            Placement {
                priority_revision: 0,
                job: 2,
                choice: Choice::NodeA,
                observed_at: 0
            },
            None
        )
        .await
        .unwrap()
        .0
    );
    assert!(
        e.apply(
            Placement {
                priority_revision: 0,
                job: 1,
                choice: Choice::NodeA,
                observed_at: 0
            },
            None
        )
        .await
        .unwrap()
        .0
    );
    let before = serde_json::to_value(e.data()).unwrap();
    assert!(
        !e.apply(
            Placement {
                priority_revision: 0,
                job: 2,
                choice: Choice::NodeA,
                observed_at: 0
            },
            None
        )
        .await
        .unwrap()
        .0
    );
    assert_eq!(before, serde_json::to_value(e.data()).unwrap());
    assert!(
        e.apply(
            Placement {
                priority_revision: 0,
                job: 2,
                choice: Choice::NodeB,
                observed_at: 0
            },
            None
        )
        .await
        .unwrap()
        .0
    );
    e.advance(999).await.unwrap();
    assert_eq!(e.data().nodes[0].used_cpu, 3);
    e.advance(1000).await.unwrap();
    assert_eq!(e.data().nodes[0].used_cpu, 0);
    assert_eq!(e.data().nodes[1].used_memory_gib, 0);
    assert!(e.data().jobs.iter().all(|j| j.phase == JobPhase::Completed));
    e.arrival(job(3, 1, 9)).await.unwrap();
    assert!(
        !e.apply(
            Placement {
                priority_revision: 0,
                job: 3,
                choice: Choice::NodeA,
                observed_at: 1000
            },
            None
        )
        .await
        .unwrap()
        .0
    );
    e.advance(6001).await.unwrap();
    assert!(
        !e.apply(
            Placement {
                priority_revision: 0,
                job: 3,
                choice: Choice::NodeB,
                observed_at: 1000
            },
            None
        )
        .await
        .unwrap()
        .0
    );
    assert!(
        e.apply(
            Placement {
                priority_revision: 0,
                job: 3,
                choice: Choice::Defer,
                observed_at: 6001
            },
            None
        )
        .await
        .unwrap()
        .0
    );
    assert_eq!(e.data().jobs[2].phase, JobPhase::Queued);
    invariant(&Phase::Scheduling, &e.data()).unwrap();
}
#[tokio::test]
async fn impossible_and_excess_arrivals_are_rejected_without_reservations() {
    let mut e = Engine::new().unwrap();
    e.arrival(job(1, 32, 64)).await.unwrap();
    assert_eq!(e.data().jobs[0].phase, JobPhase::Rejected);
    for id in 2..=131 {
        e.arrival(job(id, 1, 1)).await.unwrap();
    }
    assert_eq!(
        e.data()
            .jobs
            .iter()
            .filter(|j| j.phase == JobPhase::Queued)
            .count(),
        128
    );
    assert_eq!(
        e.data()
            .jobs
            .iter()
            .filter(|j| j.phase == JobPhase::Rejected)
            .count(),
        3
    );
    assert!(e
        .data()
        .nodes
        .iter()
        .all(|n| n.used_cpu == 0 && n.used_memory_gib == 0));
}
fn config(rate: f64, cpu: u32) -> ClientConfig {
    ClientConfig {
        rate,
        cpu,
        memory_gib: 2,
        duration_ms: 4000,
        enabled: true,
    }
}
#[tokio::test]
async fn client_controls_only_affect_future_jobs_and_removal_keeps_existing_work() {
    let mut s = Session::new(42, None, JevSettings::default()).unwrap();
    // Isolate this client's arrivals from the playground defaults.
    for id in 1..3 {
        s.command(Command::Client { id, config: config(0., 1) }).await.unwrap();
    }
    s.command(Command::Client {
        id: 0,
        config: config(2., 1),
    })
    .await
    .unwrap();
    s.command(Command::Step).await.unwrap();
    assert_eq!(s.data().jobs.len(), 2);
    assert!(s.data().jobs.iter().all(|j| j.cpu == 1));
    s.command(Command::Client {
        id: 0,
        config: config(1., 4),
    })
    .await
    .unwrap();
    s.command(Command::Step).await.unwrap();
    assert_eq!(s.data().jobs[0].cpu, 1);
    assert_eq!(s.data().jobs[2].cpu, 4);
    s.command(Command::RemoveClient { id: 0 }).await.unwrap();
    let before = s.data().jobs.len();
    s.command(Command::Step).await.unwrap();
    assert_eq!(s.data().jobs.len(), before);
    s.command(Command::Step).await.unwrap();
    s.command(Command::Step).await.unwrap();
    assert!(s
        .data()
        .jobs
        .iter()
        .any(|j| j.client == 0 && j.phase == JobPhase::Completed));
}
#[tokio::test]
async fn baseline_runs_are_seeded_step_size_independent_and_conserve_resources() {
    let mut a = Session::new(42, None, JevSettings::default()).unwrap();
    let mut b = Session::new(42, None, JevSettings::default()).unwrap();
    a.command(Command::Play).await.unwrap();
    b.command(Command::Play).await.unwrap();
    for _ in 0..200 {
        a.tick(50).await.unwrap();
    }
    for _ in 0..10 {
        b.tick(1000).await.unwrap();
    }
    assert_eq!(
        serde_json::to_value(a.data()).unwrap(),
        serde_json::to_value(b.data()).unwrap()
    );
    invariant(&Phase::Scheduling, &a.data()).unwrap();
    let d = a.data();
    let evidence = judge::evidence(&d);
    if let Some(e) = evidence {
        let json = serde_json::to_string(&e).unwrap();
        assert!(!json.contains("actual_duration"));
        assert!(!json.contains("finish_at"));
    }
}
#[tokio::test]
async fn controls_validate_bounds_and_support_eight_independent_clients() {
    let mut s = Session::new(1, None, JevSettings::default()).unwrap();
    let original = s.view().clients[0].config.rate;
    for rate in [0.3, 1.5] {
        assert!(s.command(Command::Client { id: 0, config: config(rate, 1) }).await.is_err());
        assert_eq!(s.view().clients[0].config.rate, original);
    }
    assert!(s.view().clients.iter().all(|c| c.config.rate.fract() == 0.));
    assert!(s
        .command(Command::Client {
            id: 0,
            config: config(-1., 1)
        })
        .await
        .is_err());
    for _ in 0..5 {
        s.command(Command::AddClient).await.unwrap();
    }
    assert_eq!(s.view().clients.len(), 8);
    assert!(s.command(Command::AddClient).await.is_err());
    assert!(s
        .command(Command::Policy {
            policy: Policy::Jev
        })
        .await
        .is_err());
    s.command(Command::Client {
        id: 0,
        config: config(0., 1),
    })
    .await
    .unwrap();
    for _ in 0..5 {
        s.command(Command::Step).await.unwrap();
    }
    assert!(!s.data().jobs.iter().any(|j| j.client == 0));
}
struct Mock {
    calls: AtomicUsize,
    delay: Duration,
}
impl Evaluator for Mock {
    fn evaluate(&self, e: Evidence) -> Evaluation<'_> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(self.delay).await;
            Inference {
                choice: Some(e.legal_choices[0]),
                confidence: Some(0.8),
                probabilities: Default::default(),
                model: Some("jev-1.13.0".into()),
                usage: Some(typesafe_ai::Usage {
                    input_tokens: 1000,
                    output_tokens: 20,
                    extra: Default::default(),
                }),
                latency_ms: 0.,
                error: None,
            }
        })
    }
}
#[tokio::test]
async fn slow_jev_does_not_block_arrivals_and_reset_cancels_pending_placement() {
    let mock = Arc::new(Mock {
        calls: AtomicUsize::new(0),
        delay: Duration::from_secs(20),
    });
    let mut s = Session::new(42, Some(mock.clone()), JevSettings::default()).unwrap();
    // Only this client produces the six arrivals while inference is pending.
    for id in 1..3 {
        s.command(Command::Client { id, config: config(0., 1) }).await.unwrap();
    }
    s.command(Command::Client {
        id: 0,
        config: config(2., 1),
    })
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(1), async {
        for _ in 0..3 {
            s.command(Command::Step).await.unwrap();
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(s.view().queued, 6);
    assert!(s.view().pending_job.is_some());
    assert_eq!(mock.calls.load(Ordering::SeqCst), 1);
    s.command(Command::Reset).await.unwrap();
    assert!(s.view().pending_job.is_none());
    assert!(s.data().jobs.is_empty());
}
#[tokio::test]
async fn jev_scores_cost_and_pause_semantics_are_preserved() {
    let mock = Arc::new(Mock {
        calls: AtomicUsize::new(0),
        delay: Duration::from_millis(1),
    });
    let settings = JevSettings {
        dispatch_interval: Duration::ZERO,
        ..JevSettings::default()
    };
    let mut s = Session::new(42, Some(mock.clone()), settings).unwrap();
    s.command(Command::Client {
        id: 0,
        config: config(1., 1),
    })
    .await
    .unwrap();
    s.command(Command::Step).await.unwrap();
    tokio::time::sleep(Duration::from_millis(15)).await;
    assert_eq!(s.view().running, 0);
    assert!((s.view().cost.estimated_usd - 0.000042).abs() < 1e-12);
    s.command(Command::Step).await.unwrap();
    assert_eq!(s.view().running, 1);
    assert_eq!(
        s.view().decisions[0].result.as_ref().unwrap().confidence,
        Some(0.8)
    );
    for _ in 0..5 {
        s.command(Command::Step).await.unwrap();
        tokio::time::sleep(Duration::from_millis(15)).await;
    }
    assert!(mock.calls.load(Ordering::SeqCst) > 1);
    let priced_calls = s.view().cost.priced_calls;
    s.command(Command::Reset).await.unwrap();
    assert_eq!(s.view().calls, 0);
    assert_eq!(s.view().cost.priced_calls, priced_calls);
}

#[tokio::test]
async fn priority_edits_cancel_pending_and_preserve_work_and_arrival_clocks() {
    use reflex_sim::scheduler::Priority;
    let mock = Arc::new(Mock {
        calls: AtomicUsize::new(0),
        delay: Duration::from_secs(20),
    });
    let mut s = Session::new(42, Some(mock), JevSettings::default()).unwrap();
    for id in 0..3 {
        s.command(Command::Client {
            id,
            config: config(1., 1),
        })
        .await
        .unwrap();
    }
    s.command(Command::Step).await.unwrap();
    let before = s.data();
    assert!(s.view().pending_job.is_some());
    s.command(Command::Priority {
        id: 1,
        priority: Priority::Critical,
    })
    .await
    .unwrap();
    assert!(s.view().pending_job.is_none());
    assert_eq!(s.data().at_ms, before.at_ms);
    assert_eq!(
        serde_json::to_value(s.data().jobs).unwrap(),
        serde_json::to_value(before.jobs).unwrap()
    );
    let e = judge::evidence(&s.data()).unwrap();
    assert_eq!(e.candidate.client, 1);
    assert_eq!(e.candidate.priority, Priority::Critical);
    assert_eq!(e.candidates.len(), 3);
    s.command(Command::Step).await.unwrap();
    assert_eq!(
        s.data()
            .jobs
            .iter()
            .filter(|j| j.client == 1)
            .map(|j| j.arrived_at)
            .collect::<Vec<_>>(),
        vec![1000, 2000]
    );
    s.command(Command::Priority {
        id: 1,
        priority: Priority::Normal,
    })
    .await
    .unwrap();
    assert_eq!(judge::evidence(&s.data()).unwrap().candidate.client, 0);
    assert!(s
        .command(Command::Priority {
            id: 99,
            priority: Priority::Critical
        })
        .await
        .is_err());
    s.command(Command::Priority {
        id: 1,
        priority: Priority::High,
    })
    .await
    .unwrap();
    s.command(Command::Reset).await.unwrap();
    assert_eq!(s.data().priority(1), Priority::High);
}

#[tokio::test]
async fn guards_enforce_client_fifo_priority_revision_and_oldest_feasible_aging() {
    use reflex_sim::scheduler::Priority;
    let mut e = Engine::new().unwrap();
    e.arrival(job(1, 1, 2)).await.unwrap();
    e.arrival(job(2, 1, 2)).await.unwrap(); // Same client: cannot overtake #1.
    let mut third = job(3, 1, 2);
    third.client = 1;
    e.arrival(third).await.unwrap();
    e.set_priority(1, Priority::Critical).await.unwrap();
    let action = |job, choice, at, revision| Placement {
        job,
        choice,
        observed_at: at,
        priority_revision: revision,
    };
    assert!(
        !e.apply(action(3, Choice::NodeA, 0, 0), None)
            .await
            .unwrap()
            .0
    );
    assert!(
        !e.apply(action(2, Choice::NodeA, 0, 1), None)
            .await
            .unwrap()
            .0
    );
    assert!(
        e.apply(action(3, Choice::NodeA, 0, 1), None)
            .await
            .unwrap()
            .0
    );
    e.set_priority(1, Priority::Normal).await.unwrap();
    assert_eq!(e.data().jobs[2].phase, JobPhase::Running); // No preemption.
    e.advance(30_000).await.unwrap();
    let mut fourth = job(4, 1, 2);
    fourth.client = 1;
    fourth.arrived_at = 30_000;
    e.arrival(fourth).await.unwrap();
    e.set_priority(1, Priority::Critical).await.unwrap();
    let evidence = judge::evidence(&e.data()).unwrap();
    assert_eq!(
        evidence.candidates.iter().map(|j| j.id).collect::<Vec<_>>(),
        vec![1]
    );
    assert!(!evidence.legal_choices.contains(&Choice::Defer));
    assert!(
        !e.apply(action(4, Choice::NodeA, 30_000, 3), None)
            .await
            .unwrap()
            .0
    );
    assert!(
        !e.apply(action(1, Choice::Defer, 30_000, 3), None)
            .await
            .unwrap()
            .0
    );
    assert!(
        e.apply(action(1, Choice::NodeA, 30_000, 3), None)
            .await
            .unwrap()
            .0
    );
    assert_eq!(judge::evidence(&e.data()).unwrap().candidate.id, 2);
    invariant(&Phase::Scheduling, &e.data()).unwrap();
}

#[tokio::test]
async fn blocked_client_head_does_not_hold_other_clients_or_expose_its_younger_jobs() {
    let mut e = Engine::new().unwrap();
    let mut running = job(1, 16, 32);
    running.actual_duration_ms = 60_000;
    e.arrival(running).await.unwrap();
    assert!(
        e.apply(
            Placement {
                job: 1,
                choice: Choice::NodeD,
                observed_at: 0,
                priority_revision: 0
            },
            None
        )
        .await
        .unwrap()
        .0
    );
    e.arrival(job(2, 16, 32)).await.unwrap();
    e.arrival(job(3, 1, 1)).await.unwrap();
    let mut other = job(4, 1, 1);
    other.client = 1;
    e.arrival(other).await.unwrap();
    e.advance(30_000).await.unwrap();
    assert_eq!(
        judge::evidence(&e.data())
            .unwrap()
            .candidates
            .iter()
            .map(|j| j.id)
            .collect::<Vec<_>>(),
        vec![4]
    );
    assert!(
        e.apply(
            Placement {
                job: 4,
                choice: Choice::NodeA,
                observed_at: 30_000,
                priority_revision: 0
            },
            None
        )
        .await
        .unwrap()
        .0
    );
    assert!(judge::evidence(&e.data()).is_none());
}

struct PickOther;
impl Evaluator for PickOther {
    fn evaluate(&self, e: Evidence) -> Evaluation<'_> {
        Box::pin(async move {
            let c = e
                .legal_choices
                .iter()
                .find(|c| matches!(c, Choice::Place { .. }))
                .copied()
                .unwrap();
            let mut r = Inference::failed("");
            r.error = None;
            r.choice = Some(c);
            r
        })
    }
}
#[tokio::test]
async fn jev_can_select_another_client_and_records_selected_request() {
    let mut s = Session::new(
        42,
        Some(Arc::new(PickOther)),
        JevSettings {
            ..Default::default()
        },
    )
    .unwrap();
    for id in 0..3 {
        s.command(Command::Client {
            id,
            config: config(1., 1),
        })
        .await
        .unwrap();
    }
    s.command(Command::Step).await.unwrap();
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
    s.command(Command::Step).await.unwrap();
    let view = s.view();
    let decision = &view.decisions[0];
    assert_eq!(decision.job, 2);
    assert_eq!(decision.status, "placed");
    assert_eq!(s.data().jobs[0].phase, JobPhase::Queued);
    assert_eq!(s.data().jobs[1].phase, JobPhase::Running);
    let stats = view.client_stats.iter().find(|s| s.client == 1).unwrap();
    assert_eq!(stats.started, 1);
    assert_eq!(stats.mean_wait_ms, Some(1000.));
    assert_eq!(stats.p95_wait_ms, Some(1000));
}

#[tokio::test]
async fn sole_aged_placement_uses_guards_without_another_jev_call() {
    let mock = Arc::new(Mock {
        calls: AtomicUsize::new(0),
        delay: Duration::from_secs(20),
    });
    let mut s = Session::new(
        42,
        Some(mock.clone()),
        JevSettings {
            ..Default::default()
        },
    )
    .unwrap();
    for id in 0..3 {
        s.command(Command::Client {
            id,
            config: config(if id == 0 { 1. } else { 0. }, 16),
        })
        .await
        .unwrap();
    }
    for _ in 0..31 {
        s.command(Command::Step).await.unwrap();
    }
    assert_eq!(s.view().calls, 1);
    s.command(Command::Priority {
        id: 0,
        priority: reflex_sim::scheduler::Priority::Critical,
    }).await.unwrap();
    s.command(Command::Step).await.unwrap();
    assert_eq!(s.view().calls, 1);
    assert_eq!(s.data().jobs[0].phase, JobPhase::Running);
    assert_eq!(s.data().jobs[0].node, Some(3));
    assert_eq!(s.view().decisions[0].status, "placed");
}

#[tokio::test]
async fn uncapped_rates_allow_five_per_second_and_submillisecond_arrivals() {
    for (rate, elapsed, expected) in [(5., 1000, 5), (2000., 2, 4)] {
        let mut s = Session::new(42, None, JevSettings::default()).unwrap();
        for id in 0..3 {
            s.command(Command::Client {
                id,
                config: config(if id == 0 { rate } else { 0. }, 1),
            })
            .await
            .unwrap();
        }
        s.command(Command::Play).await.unwrap();
        tokio::time::timeout(Duration::from_secs(2), s.tick(elapsed))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(s.data().jobs.len(), expected);
        assert_eq!(s.data().at_ms, elapsed);
        invariant(&Phase::Scheduling, &s.data()).unwrap();
    }
}

#[tokio::test]
async fn lag_reports_current_backlog_and_recent_starts_without_fabricating_zero_starts() {
    let mut e = Engine::new().unwrap();
    e.arrival(job(1, 1, 2)).await.unwrap();
    let mut other = job(2, 1, 2);
    other.client = 1;
    e.arrival(other).await.unwrap();
    e.advance(3000).await.unwrap();
    let lag = e.data().client_lag(0);
    assert_eq!(lag.oldest_wait_ms, 3000);
    assert_eq!(lag.queued, 1);
    assert_eq!(lag.recent_mean_start_lag_ms, None);
    assert!(
        e.apply(
            Placement {
                job: 1,
                choice: Choice::NodeA,
                observed_at: 3000,
                priority_revision: 0
            },
            None
        )
        .await
        .unwrap()
        .0
    );
    let lag = e.data().client_lag(0);
    assert_eq!(lag.oldest_wait_ms, 0);
    assert_eq!(lag.queued, 0);
    assert_eq!(lag.recent_mean_start_lag_ms, Some(3000.));
    assert_eq!(lag.recent_starts, 1);
    assert_eq!(e.data().client_lag(1).oldest_wait_ms, 3000);
    e.advance(13000).await.unwrap();
    assert_eq!(e.data().client_lag(0).recent_mean_start_lag_ms, None);
    assert_eq!(e.data().client_lag(1).oldest_wait_ms, 13000);
}

#[tokio::test]
async fn lag_history_and_priority_timeline_preserve_prior_values_and_reset() {
    use reflex_sim::scheduler::Priority;
    let mock = Arc::new(Mock {
        calls: AtomicUsize::new(0),
        delay: Duration::from_secs(20),
    });
    let mut s = Session::new(42, Some(mock), JevSettings::default()).unwrap();
    s.command(Command::Client {
        id: 0,
        config: config(5., 1),
    })
    .await
    .unwrap();
    s.command(Command::Step).await.unwrap();
    s.command(Command::Priority {
        id: 0,
        priority: Priority::Critical,
    })
    .await
    .unwrap();
    s.command(Command::Step).await.unwrap();
    let v = s.view();
    assert_eq!(v.history[0].clients[0].priority, Priority::Normal);
    assert_eq!(v.history[1].clients[0].priority, Priority::Critical);
    assert_eq!(v.live_lag[0].oldest_wait_ms, 1800);
    assert_eq!(v.priority_changes[0].at_ms, 1000);
    s.command(Command::RemoveClient { id: 0 }).await.unwrap();
    assert!(s
        .view()
        .live_lag
        .iter()
        .any(|c| c.client == 0 && c.queued == 10));
    s.command(Command::Reset).await.unwrap();
    assert!(s.view().history.is_empty());
    assert!(s.view().priority_changes.is_empty());
}

#[tokio::test]
async fn resource_history_preserves_reservations_after_jobs_finish_and_clears_on_reset() {
    let mut s = Session::new(42, None, JevSettings::default()).unwrap();
    for id in 0..3 {
        s.command(Command::Client {
            id,
            config: ClientConfig {
                rate: if id == 0 { 1. } else { 0. },
                cpu: 1,
                memory_gib: 2,
                duration_ms: 1000,
                enabled: true,
            },
        })
        .await
        .unwrap();
    }
    s.command(Command::Step).await.unwrap();
    let first = s.view().history[0].nodes.clone();
    assert_eq!(first.iter().map(|n| n.used_cpu).sum::<u32>(), 1);
    assert_eq!(first.iter().map(|n| n.used_memory_gib).sum::<u32>(), 2);
    s.command(Command::Client {
        id: 0,
        config: config(0., 1),
    })
    .await
    .unwrap();
    for _ in 0..3 {
        s.command(Command::Step).await.unwrap();
    }
    let view = s.view();
    assert!(view
        .nodes
        .iter()
        .all(|n| n.used_cpu == 0 && n.used_memory_gib == 0));
    assert_eq!(
        serde_json::to_value(&view.history[0].nodes).unwrap(),
        serde_json::to_value(&first).unwrap()
    );
    assert!(view
        .history
        .last()
        .unwrap()
        .nodes
        .iter()
        .all(|n| n.used_cpu == 0 && n.used_memory_gib == 0));
    assert!(view
        .history
        .iter()
        .flat_map(|p| &p.nodes)
        .all(|n| n.used_cpu <= n.cpu && n.used_memory_gib <= n.memory_gib));
    s.command(Command::Reset).await.unwrap();
    assert!(s.view().history.is_empty());
}


struct DeferAll;
impl Evaluator for DeferAll {
    fn evaluate(&self, _: Evidence) -> Evaluation<'_> {
        Box::pin(async {
            let mut result = Inference::failed("");
            result.error = None;
            result.choice = Some(Choice::Defer);
            result
        })
    }
}
#[tokio::test]
async fn evaluations_continue_past_180_calls_but_stop_at_simulation_timeout() {
    let mut s = Session::new(
        42,
        Some(Arc::new(DeferAll)),
        JevSettings {
            dispatch_interval: Duration::ZERO,
            ..Default::default()
        },
    ).unwrap();
    s.command(Command::Client { id: 0, config: config(1., 1) }).await.unwrap();
    s.command(Command::Play).await.unwrap();
    for _ in 0..400 {
        s.tick(50).await.unwrap();
        tokio::task::yield_now().await;
    }
    assert!(s.view().calls > 180);
    s.command(Command::Pause).await.unwrap();
    let calls = s.view().calls;
    s.tick(1000).await.unwrap();
    assert_eq!(s.view().calls, calls);
    s.command(Command::Play).await.unwrap();
    let horizon = s.view().horizon_ms;
    s.tick(horizon).await.unwrap();
    assert_eq!(s.data().at_ms, horizon);
    assert!(s.view().paused);
    assert_eq!(s.view().calls, calls);
    s.command(Command::Step).await.unwrap();
    assert_eq!(s.view().calls, calls);
}

#[tokio::test]
async fn live_defaults_have_capacity_headroom_until_the_user_increases_load() {
    for policy in [Policy::FirstFit, Policy::BestFit] {
        let mut s = Session::new(42, None, JevSettings::default()).unwrap();
        s.command(Command::Policy { policy }).await.unwrap();
        s.command(Command::Play).await.unwrap();
        let mut peak_cpu = 0;
        for _ in 0..120 {
            s.tick(1000).await.unwrap();
            let d = s.data();
            peak_cpu = peak_cpu.max(d.nodes.iter().map(|n| n.used_cpu).sum::<u32>());
            assert!(d.jobs.iter().all(|j| j.phase != JobPhase::Rejected));
            assert_eq!(
                s.view().queued,
                0,
                "default arrivals should drain without a backlog"
            );
        }
        assert!(
            peak_cpu > 0 && peak_cpu <= 18,
            "default work should leave at least half the pool free"
        );
        let mut config = s.view().clients[2].config.clone();
        config.rate = 12.;
        s.command(Command::Client { id: 2, config }).await.unwrap();
        for _ in 0..10 {
            s.tick(1000).await.unwrap();
        }
        assert!(
            s.view().queued > 0,
            "raising traffic should create visible contention"
        );
    }
}
