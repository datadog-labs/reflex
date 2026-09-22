use opentelemetry::metrics::MeterProvider;
use reflex_sim::{
    playground::inference::JevSettings,
    recovery::{
        engine::{Action, ClientConfig, Engine, Event, Faults, Proposal},
        Command, Policy, Session,
    },
};
#[allow(dead_code)]
#[path = "support/metrics.rs"]
mod metrics;
use metrics::{count, gauge, histogram, Capture};
async fn ticks(e: &mut Engine, n: usize) {
    for _ in 0..n {
        e.event(Event::Tick).await.unwrap();
    }
}
async fn clients(e: &mut Engine, rate: f64, cost_ms: u64, essential_pct: u8) {
    for id in 0..3 {
        e.event(Event::Client {
            id,
            config: ClientConfig {
                rate,
                cost_ms,
                essential_pct,
                enabled: true,
            },
        })
        .await
        .unwrap();
    }
}
async fn apply(e: &mut Engine, action: Action) -> bool {
    let d = e.data();
    e.apply(
        Proposal {
            action,
            observed_at: d.at_ms,
            revision: d.revision,
        },
        Some(1.),
    )
    .await
    .unwrap()
    .0
}
#[tokio::test]
async fn retry_attempts_preserve_one_terminal_client_result() {
    let c = Capture::new();
    let mut e = Engine::with_meter(42, Policy::Fixed, c.provider.meter("recovery")).unwrap();
    clients(&mut e, 10., 50, 100).await;
    e.event(Event::Fault {
        replica: 0,
        faults: Faults {
            error_rate: 1.,
            ..Default::default()
        },
    })
    .await
    .unwrap();
    ticks(&mut e, 40).await;
    assert!(apply(&mut e, Action::SetRetryBudget { enabled: true }).await);
    ticks(&mut e, 200).await;
    clients(&mut e, 0., 50, 100).await;
    ticks(&mut e, 65).await;
    let d = e.data();
    let m = c.read();
    assert!(d.requests.is_empty());
    assert_eq!(count(&m, "http.client.requests", &[]), d.totals.arrivals);
    assert_eq!(
        count(&m, "http.client.requests", &[("error", "false")]),
        d.totals.succeeded
    );
    assert_eq!(
        count(&m, "http.client.requests", &[("error", "true")]),
        d.totals.failed + d.totals.rejected
    );
    assert!(
        count(
            &m,
            "http.client.requests",
            &[("retried", "true"), ("error", "false")]
        ) > 0
    );
    assert_eq!(
        count(&m, "http.server.requests", &[]),
        d.totals.arrivals + d.totals.retries
    );
    assert_eq!(
        count(&m, "recovery.retries", &[("outcome", "attempted")]),
        d.totals.retries
    );
    for reason in ["disabled", "budget_exhausted"] {
        assert!(count(&m, "recovery.retries", &[("reason", reason)]) > 0);
    }
    assert_eq!(
        histogram(&m, "http.client.request.duration", &[]).0,
        d.totals.arrivals
    );
    assert_eq!(
        histogram(&m, "http.server.queue.wait", &[]).0,
        d.totals.arrivals + d.totals.retries
    );
    assert!(histogram(&m, "http.client.request.duration", &[("retried", "true")]).1 > 0.);
    assert_eq!(gauge(&m, "http.client.in_flight", &[]), vec![0.; 3]);
    // Reads, exports, evaluation errors and rejected transitions cannot recount old events.
    e.data();
    e.evaluation_error("unavailable").await.unwrap();
    assert!(!apply(&mut e, Action::AddToServing { replica: 3 }).await);
    assert_eq!(
        count(&c.read(), "http.client.requests", &[]),
        d.totals.arrivals
    );
    let encoded = serde_json::to_string(&e.data()).unwrap();
    assert!(!encoded.contains("metric_events"));
}
#[tokio::test]
async fn partition_timeouts_and_rejections_do_not_fabricate_server_responses() {
    let c = Capture::new();
    let mut e = Engine::with_meter(42, Policy::Fixed, c.provider.meter("recovery")).unwrap();
    clients(&mut e, 40., 1000, 0).await;
    for replica in 0..3 {
        e.event(Event::Fault {
            replica,
            faults: Faults {
                client_partition: true,
                ..Default::default()
            },
        })
        .await
        .unwrap();
    }
    ticks(&mut e, 80).await;
    let m = c.read();
    assert_eq!(count(&m, "http.server.requests", &[]), 0);
    assert_eq!(histogram(&m, "http.server.queue.wait", &[]).0, 0);
    assert!(count(&m, "http.client.requests", &[("reason", "timeout")]) > 0);
    assert!(count(&m, "http.client.requests", &[("reason", "queue_full")]) > 0);
    assert_eq!(gauge(&m, "http.server.active", &[]), vec![0.; 4]);
    assert!(
        gauge(&m, "recovery.replica.requests.outstanding", &[])
            .iter()
            .sum::<f64>()
            > 0.
    );
    assert!(gauge(&m, "recovery.replica.heartbeat.age", &[])
        .iter()
        .any(|v| *v > 0.));
    assert!(
        count(
            &m,
            "http.client.requests",
            &[("reason", "timeout"), ("replica", "replica_a")]
        ) > 0
    );

    assert!(
        apply(
            &mut e,
            Action::SetServingMode {
                essential_only: true
            }
        )
        .await
    );
    ticks(&mut e, 20).await;
    assert!(
        count(
            &c.read(),
            "http.client.requests",
            &[("reason", "essential_only")]
        ) > 0
    );
    let admission = c.read();
    for replica in ["replica_a", "replica_b", "replica_c", "replica_d"] {
        assert_eq!(
            count(
                &admission,
                "http.client.requests",
                &[("reason", "essential_only"), ("replica", replica)]
            ),
            0
        );
    }
    clients(&mut e, 0., 1000, 0).await;
    ticks(&mut e, 65).await;
    let d = e.data();
    assert_eq!(
        count(&c.read(), "http.client.requests", &[]),
        d.totals.arrivals
    );
    assert_eq!(
        gauge(&c.read(), "recovery.serving.essential_only", &[]),
        vec![1.]
    );
}
#[tokio::test]
async fn rebuild_bytes_and_terminal_outcomes_are_recorded_once() {
    let c = Capture::new();
    for outcome in ["completed", "failed", "cancelled"] {
        let mut e = Engine::with_meter(42, Policy::Fixed, c.provider.meter("recovery")).unwrap();
        clients(&mut e, 0., 50, 100).await;
        e.event(Event::Fault {
            replica: 0,
            faults: Faults {
                crashed: true,
                ..Default::default()
            },
        })
        .await
        .unwrap();
        ticks(&mut e, 10).await;
        assert!(
            apply(
                &mut e,
                Action::StartRebuild {
                    source: 1,
                    target: 3
                }
            )
            .await
        );
        ticks(&mut e, 20).await;
        assert_eq!(
            gauge(
                &c.read(),
                "recovery.rebuild.active",
                &[
                    ("source", "replica_b"),
                    ("target", "replica_d"),
                    ("phase", "rebuilding")
                ]
            ),
            vec![1.]
        );
        match outcome {
            "failed" => e
                .event(Event::Fault {
                    replica: 1,
                    faults: Faults {
                        transfer_partition: true,
                        ..Default::default()
                    },
                })
                .await
                .unwrap(),
            "cancelled" => assert!(apply(&mut e, Action::CancelRebuild).await),
            _ => {}
        }
        ticks(&mut e, 600).await;
        let r = e.data().recovery.unwrap();
        assert_eq!(r.phase, outcome);
        let m = c.read();
        assert_eq!(count(&m, "recovery.rebuilds", &[("outcome", outcome)]), 1);
        let (n, seconds) = histogram(&m, "recovery.rebuild.duration", &[("outcome", outcome)]);
        assert_eq!(n, 1);
        assert_eq!(
            seconds,
            (r.finished_at.unwrap() - r.started_at) as f64 / 1000.
        );
        assert!(gauge(&m, "recovery.rebuild.active", &[])
            .iter()
            .all(|v| *v == 0.));
        ticks(&mut e, 20).await;
        assert_eq!(
            count(&c.read(), "recovery.rebuilds", &[("outcome", outcome)]),
            1
        );
    }
    assert_eq!(count(&c.read(), "recovery.rebuild.bytes", &[]), 108_000_000);
}
#[tokio::test]
async fn paused_gauges_survive_and_reset_releases_old_snapshots() {
    let c = Capture::new();
    let mut s = Session::with_meter(
        42,
        None,
        JevSettings::default(),
        c.provider.meter("recovery"),
    )
    .unwrap();
    s.command(Command::Step).await.unwrap();
    let arrivals = s.data().totals.arrivals;
    let completed = count(&c.read(), "http.client.requests", &[]);
    assert!(completed > 0 && completed <= arrivals);
    assert_eq!(gauge(&c.read(), "recovery.replica.serving", &[]).len(), 4);
    s.command(Command::Reset).await.unwrap();
    assert_eq!(count(&c.read(), "http.client.requests", &[]), completed);
    assert_eq!(gauge(&c.read(), "http.client.in_flight", &[]), vec![0.; 3]);
    assert_eq!(gauge(&c.read(), "recovery.replica.serving", &[]).len(), 4);
    drop(s);
    assert!(gauge(&c.read(), "recovery.replica.serving", &[]).is_empty());
}

#[tokio::test]
async fn received_queue_rejections_and_expired_waiters_are_accounted_once() {
    let c = Capture::new();
    let mut e = Engine::with_meter(42, Policy::Fixed, c.provider.meter("recovery")).unwrap();
    clients(&mut e, 40., 1000, 100).await;
    for replica in 0..3 {
        e.event(Event::Fault {
            replica,
            faults: Faults {
                slowdown: 10.,
                ..Default::default()
            },
        })
        .await
        .unwrap();
    }
    ticks(&mut e, 80).await;
    clients(&mut e, 0., 1000, 100).await;
    ticks(&mut e, 65).await;
    let m = c.read();
    let requests = count(&m, "http.server.requests", &[]);
    assert_eq!(requests, e.data().totals.arrivals);
    assert!(count(&m, "http.server.requests", &[("http.status_code", "503")]) > 0);
    assert!(count(&m, "http.server.requests", &[("outcome", "timeout")]) > 0);
    assert_eq!(
        count(
            &m,
            "http.server.requests",
            &[("outcome", "timeout"), ("http.status_code", "500")]
        ),
        0
    );
    assert_eq!(histogram(&m, "http.server.queue.wait", &[]).0, requests);
    assert!(histogram(&m, "http.server.queue.wait", &[]).1 > 0.);
    assert_eq!(gauge(&m, "http.server.queue.depth", &[]), vec![0.; 4]);
    assert_eq!(gauge(&m, "http.server.active", &[]), vec![0.; 4]);
    assert_eq!(
        gauge(&m, "recovery.replica.requests.outstanding", &[]),
        vec![0.; 4]
    );
    assert!(
        count(
            &m,
            "http.client.requests",
            &[("reason", "timeout"), ("replica", "replica_a")]
        ) > 0
    );
}
#[tokio::test]
async fn exhausted_attempts_and_missing_alternatives_have_distinct_suppression_reasons() {
    let c = Capture::new();
    let mut e = Engine::with_meter(42, Policy::Fixed, c.provider.meter("recovery")).unwrap();
    clients(&mut e, 10., 50, 100).await;
    assert!(apply(&mut e, Action::SetRetryBudget { enabled: true }).await);
    for replica in 0..3 {
        e.event(Event::Fault {
            replica,
            faults: Faults {
                error_rate: 1.,
                ..Default::default()
            },
        })
        .await
        .unwrap();
    }
    ticks(&mut e, 100).await;
    assert!(
        count(
            &c.read(),
            "recovery.retries",
            &[("reason", "attempt_limit")]
        ) > 0
    );
    assert!(apply(&mut e, Action::RemoveFromServing { replica: 1 }).await);
    ticks(&mut e, 20).await;
    assert!(apply(&mut e, Action::RemoveFromServing { replica: 2 }).await);
    ticks(&mut e, 100).await;
    assert!(
        count(
            &c.read(),
            "recovery.retries",
            &[("reason", "no_alternative")]
        ) > 0
    );
}
