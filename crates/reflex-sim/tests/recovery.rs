use reflex_sim::{
    playground::inference::JevSettings,
    recovery::{
        self,
        engine::{Action, ClientConfig, Engine, Event, Faults, Lifecycle, Phase, Proposal},
        judge::{self, Evaluation, Evaluator, Evidence, Inference},
        Command, Policy, Scenario, Session,
    },
};
use std::{sync::Arc, time::Duration};
async fn ticks(e: &mut Engine, n: u64) {
    for _ in 0..n {
        e.event(Event::Tick).await.unwrap();
    }
}
async fn apply(e: &mut Engine, a: Action) -> (bool, String) {
    let d = e.data();
    e.apply(
        Proposal {
            action: a,
            observed_at: d.at_ms,
            revision: d.revision,
        },
        Some(0.9),
    )
    .await
    .unwrap()
}
async fn crash(e: &mut Engine, id: usize) {
    e.event(Event::Fault {
        replica: id,
        faults: Faults {
            crashed: true,
            ..Default::default()
        },
    })
    .await
    .unwrap();
    ticks(e, 10).await;
}
#[tokio::test]
async fn guards_recheck_readiness_revision_and_single_rebuild() {
    let mut e = Engine::new(42).unwrap();
    assert!(!apply(&mut e, Action::AddToServing { replica: 3 }).await.0);
    let d = e.data();
    let old = Proposal {
        action: Action::SetRetryBudget { enabled: true },
        observed_at: d.at_ms,
        revision: d.revision,
    };
    crash(&mut e, 0).await;
    assert!(!e.apply(old, None).await.unwrap().0);
    assert!(
        apply(&mut e, Action::RemoveFromServing { replica: 0 })
            .await
            .0
    );
    ticks(&mut e, 20).await;
    assert!(
        apply(
            &mut e,
            Action::StartRebuild {
                source: 1,
                target: 3
            }
        )
        .await
        .0
    );
    ticks(&mut e, 20).await;
    assert!(
        !apply(
            &mut e,
            Action::StartRebuild {
                source: 2,
                target: 0
            }
        )
        .await
        .0
    );
    assert!(!apply(&mut e, Action::AddToServing { replica: 3 }).await.0);
    let before = e.data().revision;
    assert!(
        !apply(&mut e, Action::RemoveFromServing { replica: 99 })
            .await
            .0
    );
    assert_eq!(before, e.data().revision);
    let now = e.data();
    assert!(
        !e.apply(
            Proposal {
                action: Action::KeepCurrentPlan,
                observed_at: now.at_ms + 1,
                revision: now.revision
            },
            None
        )
        .await
        .unwrap()
        .0
    );
    ticks(&mut e, 500).await;
    assert_eq!(e.data().replicas[3].phase, Lifecycle::Ready);
    assert!(!e.data().replicas[3].serving);
    assert_eq!(e.data().totals.rebuilds, 1);
    assert!(apply(&mut e, Action::AddToServing { replica: 3 }).await.0);
}
#[tokio::test]
async fn last_serving_replica_and_transfer_limits_are_enforced() {
    let mut e = Engine::new(9).unwrap();
    ticks(&mut e, 20).await;
    assert!(
        apply(&mut e, Action::RemoveFromServing { replica: 0 })
            .await
            .0
    );
    ticks(&mut e, 20).await;
    assert!(
        apply(&mut e, Action::RemoveFromServing { replica: 1 })
            .await
            .0
    );
    ticks(&mut e, 20).await;
    assert!(
        !apply(&mut e, Action::RemoveFromServing { replica: 2 })
            .await
            .0
    );
    crash(&mut e, 0).await;
    ticks(&mut e, 20).await;
    assert!(
        apply(
            &mut e,
            Action::StartRebuild {
                source: 2,
                target: 3
            }
        )
        .await
        .0
    );
    ticks(&mut e, 20).await;
    e.event(Event::Bandwidth(4.)).await.unwrap();
    assert!(!apply(&mut e, Action::SetRebuildRate { high: true }).await.0);
    e.event(Event::Fault {
        replica: 2,
        faults: Faults {
            transfer_partition: true,
            ..Default::default()
        },
    })
    .await
    .unwrap();
    ticks(&mut e, 65).await;
    let d = e.data();
    assert_eq!(d.recovery.unwrap().phase, "failed");
    assert!(!d.replicas[3].serving);
    assert_ne!(d.replicas[3].phase, Lifecycle::Ready);
}
#[tokio::test]
async fn evidence_excludes_faults_and_only_changes_after_probes() {
    let mut e = Engine::new(2).unwrap();
    e.event(Event::Fault {
        replica: 0,
        faults: Faults {
            crashed: true,
            slowdown: 7.,
            error_rate: 0.85,
            ..Default::default()
        },
    })
    .await
    .unwrap();
    let ev = judge::evidence(&e.data());
    assert!(ev.replicas[0].reachable);
    let json = serde_json::to_string(&ev).unwrap();
    for key in [
        "crashed",
        "faults",
        "slowdown",
        "error_rate",
        "scenario",
        "next_at",
        "remaining",
    ] {
        assert!(!json.contains(key), "{key}");
    }
    ticks(&mut e, 10).await;
    assert!(!judge::evidence(&e.data()).replicas[0].reachable);
}
#[tokio::test]
async fn retries_and_queues_are_bounded_and_requests_conserved() {
    let mut e = Engine::new(3).unwrap();
    for id in 0..3 {
        e.event(Event::Client {
            id,
            config: ClientConfig {
                rate: 40.,
                cost_ms: 1000,
                essential_pct: 80,
                enabled: true,
            },
        })
        .await
        .unwrap();
    }
    assert!(
        apply(&mut e, Action::SetRetryBudget { enabled: true })
            .await
            .0
    );
    for replica in 0..3 {
        e.event(Event::Fault {
            replica,
            faults: Faults {
                error_rate: 1.,
                slowdown: 5.,
                ..Default::default()
            },
        })
        .await
        .unwrap();
    }
    ticks(&mut e, 600).await;
    let d = e.data();
    recovery::engine::invariant(&Phase::Operating, &d).unwrap();
    assert!(d.totals.retries > 0);
    assert!(d.totals.retries <= d.totals.arrivals / 10);
    assert!(d.requests.len() <= 128);
    assert!(d.totals.failed + d.totals.rejected > 0);
}
#[tokio::test]
async fn rebuild_uses_serving_capacity_and_verification_requires_connectivity() {
    async fn run(rebuild: bool) -> engine_data::Summary {
        let mut e = Engine::new(10).unwrap();
        for id in 0..3 {
            e.event(Event::Client {
                id,
                config: ClientConfig {
                    rate: 25.,
                    cost_ms: 300,
                    essential_pct: 100,
                    enabled: true,
                },
            })
            .await
            .unwrap();
        }
        crash(&mut e, 0).await;
        apply(&mut e, Action::RemoveFromServing { replica: 0 }).await;
        ticks(&mut e, 20).await;
        if rebuild {
            assert!(
                apply(
                    &mut e,
                    Action::StartRebuild {
                        source: 1,
                        target: 3
                    }
                )
                .await
                .0
            );
            ticks(&mut e, 20).await;
            assert!(apply(&mut e, Action::SetRebuildRate { high: true }).await.0);
        } else {
            ticks(&mut e, 20).await;
        }
        ticks(&mut e, 100).await;
        engine_data::Summary {
            success: e.data().totals.succeeded,
        }
    }
    assert!(run(true).await.success < run(false).await.success);
}
mod engine_data {
    pub struct Summary {
        pub success: u64,
    }
}
#[tokio::test]
async fn scripted_baseline_is_repeatable_across_tick_sizes() {
    async fn run(step: u64) -> serde_json::Value {
        let mut s = Session::new(7, None, JevSettings::default()).unwrap();
        s.command(Command::Scenario {
            scenario: Scenario::SecondFailure,
        })
        .await
        .unwrap();
        s.command(Command::Play).await.unwrap();
        for _ in 0..90_000 / step {
            s.tick(step).await.unwrap();
        }
        serde_json::to_value(s.data()).unwrap()
    }
    assert_eq!(run(50).await, run(1000).await);
}
struct Fake;
impl Evaluator for Fake {
    fn evaluate(&self, _: Evidence) -> Evaluation<'_> {
        Box::pin(async {
            tokio::time::sleep(Duration::from_millis(25)).await;
            let mut r = Inference::failed("provider unavailable");
            r.error = None;
            r.choice = Some(Action::SetRetryBudget { enabled: true });
            r.confidence = Some(0.8);
            r.model = Some("jev-1.13.0".into());
            r.usage = Some(typesafe_ai::Usage {
                input_tokens: 100,
                output_tokens: 5,
                extra: Default::default(),
            });
            r
        })
    }
}
#[tokio::test]
async fn inference_is_nonblocking_paused_results_wait_and_reset_keeps_cost() {
    let settings = JevSettings {
        max_evaluations: 1,
        dispatch_interval: Duration::ZERO,
        ..Default::default()
    };
    let mut s = Session::new(7, Some(Arc::new(Fake)), settings).unwrap();
    s.command(Command::Play).await.unwrap();
    s.tick(50).await.unwrap();
    s.tick(200).await.unwrap();
    assert_eq!(s.data().at_ms, 250);
    assert!(s.view().pending);
    s.command(Command::Pause).await.unwrap();
    tokio::time::sleep(Duration::from_millis(40)).await;
    assert!(s.view().response_ready);
    assert!(!s.data().retries_enabled);
    assert_eq!(s.view().cost.priced_calls, 1);
    s.command(Command::Step).await.unwrap();
    assert!(s.data().retries_enabled);
    assert_eq!(s.view().calls, 1);
    let cost = s.view().cost.estimated_usd;
    s.command(Command::Reset).await.unwrap();
    assert_eq!(s.view().cost.estimated_usd, cost);
    assert!(!s.data().retries_enabled);
    assert!(!s.view().pending);
}
#[tokio::test]
async fn client_bounds_and_policy_without_credentials() {
    let mut s = Session::new(4, None, JevSettings::default()).unwrap();
    assert!(s
        .command(Command::Policy {
            policy: Policy::Jev
        })
        .await
        .is_err());
    assert!(s.command(Command::Bandwidth { value: 17. }).await.is_err());
    for _ in 0..5 {
        s.command(Command::AddClient).await.unwrap();
    }
    assert!(s.command(Command::AddClient).await.is_err());
    assert!(s
        .command(Command::Client {
            id: 0,
            config: ClientConfig {
                rate: 41.,
                cost_ms: 150,
                essential_pct: 50,
                enabled: true
            }
        })
        .await
        .is_err());
    s.command(Command::RemoveClient { id: 0 }).await.unwrap();
    s.command(Command::Reset).await.unwrap();
    assert_eq!(s.data().clients.len(), 7);
}

#[tokio::test]
async fn verification_failure_never_admits_a_replica_and_arrival_rates_are_preserved() {
    let mut e = Engine::new(8).unwrap();
    ticks(&mut e, 20).await;
    assert_eq!(e.data().totals.arrivals, 24);
    crash(&mut e, 0).await;
    assert!(
        apply(
            &mut e,
            Action::StartRebuild {
                source: 1,
                target: 3
            }
        )
        .await
        .0
    );
    for _ in 0..520 {
        if e.data().recovery.as_ref().unwrap().phase == "verifying" {
            break;
        }
        ticks(&mut e, 1).await;
    }
    assert_eq!(e.data().recovery.as_ref().unwrap().phase, "verifying");
    assert_eq!(e.data().replicas[3].version, 0);
    e.event(Event::Fault {
        replica: 1,
        faults: Faults {
            transfer_partition: true,
            ..Default::default()
        },
    })
    .await
    .unwrap();
    ticks(&mut e, 65).await;
    assert_eq!(e.data().recovery.as_ref().unwrap().phase, "failed");
    assert_eq!(e.data().totals.rebuilds, 0);
    assert!(!apply(&mut e, Action::AddToServing { replica: 3 }).await.0);
    let d = e.data();
    assert!(
        !e.apply(
            Proposal {
                action: Action::SetRetryBudget { enabled: true },
                observed_at: 0,
                revision: d.revision
            },
            None
        )
        .await
        .unwrap()
        .0
    );
}
struct FailedEvaluator;
impl Evaluator for FailedEvaluator {
    fn evaluate(&self, _: Evidence) -> Evaluation<'_> {
        Box::pin(async { Inference::failed("provider unavailable") })
    }
}
#[tokio::test]
async fn evaluation_failure_retains_the_plan_and_is_visible() {
    let mut s = Session::new(
        1,
        Some(Arc::new(FailedEvaluator)),
        JevSettings {
            max_evaluations: 1,
            ..Default::default()
        },
    )
    .unwrap();
    s.command(Command::Play).await.unwrap();
    s.tick(50).await.unwrap();
    tokio::task::yield_now().await;
    s.tick(50).await.unwrap();
    let v = s.view();
    assert_eq!(v.decisions[0].status, "evaluation_error");
    assert!(v.decisions[0].reason.contains("existing plan retained"));
    assert!(!v.data.essential_only);
    assert!(!v.data.retries_enabled);
    assert!(v.data.recovery.is_none());
}
