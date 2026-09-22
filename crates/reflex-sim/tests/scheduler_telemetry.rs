use opentelemetry::metrics::MeterProvider;
#[allow(dead_code)]
#[path = "support/metrics.rs"]
mod metrics;
use metrics::{count, gauge, histogram, Capture};
use reflex_sim::{
    playground::inference::JevSettings,
    scheduler::{
        engine::{Choice, JobPhase},
        judge::{Evaluation, Evaluator, Evidence, Inference},
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
fn config(rate: f64, cpu: u32, duration_ms: u64) -> ClientConfig {
    ClientConfig {
        rate,
        cpu,
        memory_gib: cpu * 2,
        duration_ms,
        enabled: true,
    }
}
async fn set(session: &mut Session, id: u64, value: ClientConfig) {
    session
        .command(Command::Client { id, config: value })
        .await
        .unwrap();
}
async fn steps(session: &mut Session, count: usize) {
    for _ in 0..count {
        session.command(Command::Step).await.unwrap();
    }
}
#[tokio::test]
async fn lifecycle_metrics_match_committed_jobs_and_simulated_durations() {
    let c = Capture::new();
    let mut s = Session::with_meter(
        42,
        None,
        JevSettings::default(),
        c.provider.meter("scheduler"),
    )
    .unwrap();
    set(&mut s, 0, config(2.0, 16, 1000)).await;
    for id in [1, 2] {
        set(&mut s, id, config(0.0, 1, 1000)).await;
    }
    steps(&mut s, 7).await;
    let data = s.data();
    let metrics = c.read();
    assert_eq!(
        gauge(
            &metrics,
            "scheduler.node.cpu.reserved",
            &[("node", "node_d")]
        ),
        vec![16.0]
    );
    assert_eq!(
        gauge(
            &metrics,
            "scheduler.node.memory.reserved",
            &[("node", "node_d")]
        ),
        vec![(32u64 << 30) as f64]
    );
    assert_eq!(
        gauge(
            &metrics,
            "scheduler.node.jobs.running",
            &[("node", "node_d")]
        ),
        vec![1.0]
    );
    assert_eq!(
        gauge(
            &metrics,
            "scheduler.node.cpu.capacity",
            &[("node", "node_a")]
        ),
        vec![4.0]
    );
    assert_eq!(
        gauge(
            &metrics,
            "scheduler.node.memory.capacity",
            &[("node", "node_a")]
        ),
        vec![(8u64 << 30) as f64]
    );
    let queued: Vec<_> = data
        .jobs
        .iter()
        .filter(|j| j.phase == JobPhase::Queued)
        .collect();
    assert!(!queued.is_empty());
    assert_eq!(
        gauge(&metrics, "scheduler.queue.depth", &[("client", "client_1")]),
        vec![queued.len() as f64]
    );
    assert_eq!(
        gauge(
            &metrics,
            "scheduler.queue.oldest_age",
            &[("client", "client_1")]
        ),
        vec![(data.at_ms - queued[0].arrived_at) as f64 / 1000.0]
    );
    // A paused export preserves gauges and cannot add outcomes or histogram samples.
    let paused = c.read();
    assert_eq!(
        histogram(&metrics, "scheduler.job.cpu", &[]),
        histogram(&paused, "scheduler.job.cpu", &[])
    );
    set(&mut s, 0, config(0.0, 16, 1000)).await;
    steps(&mut s, 20).await;
    let data = s.data();
    let metrics = c.read();
    assert!(data.jobs.iter().all(|j| j.phase == JobPhase::Completed));
    let n = data.jobs.len() as u64;
    assert_eq!(
        count(
            &metrics,
            "scheduler.jobs",
            &[
                ("outcome", "completed"),
                ("error", "false"),
                ("node", "node_d")
            ]
        ),
        n
    );
    assert_eq!(
        count(&metrics, "scheduler.placements", &[("outcome", "placed")]),
        n
    );
    assert_eq!(
        histogram(&metrics, "scheduler.job.cpu", &[]),
        (n, (n * 16) as f64)
    );
    assert_eq!(
        histogram(&metrics, "scheduler.job.memory", &[]),
        (n, (n * (32u64 << 30)) as f64)
    );
    for (name, expected) in [
        (
            "scheduler.job.duration",
            data.jobs
                .iter()
                .map(|j| (j.completed_at.unwrap() - j.arrived_at) as f64 / 1000.0)
                .sum::<f64>(),
        ),
        (
            "scheduler.job.run.duration",
            data.jobs
                .iter()
                .map(|j| j.actual_duration_ms as f64 / 1000.0)
                .sum(),
        ),
        (
            "scheduler.queue.wait",
            data.jobs
                .iter()
                .map(|j| (j.started_at.unwrap() - j.arrived_at) as f64 / 1000.0)
                .sum(),
        ),
    ] {
        let (samples, sum) = histogram(&metrics, name, &[]);
        assert_eq!(samples, n);
        assert!((sum - expected).abs() < 1e-8);
    }
    assert_eq!(
        gauge(
            &metrics,
            "scheduler.node.cpu.reserved",
            &[("node", "node_d")]
        ),
        vec![0.0]
    );
    assert_eq!(
        gauge(
            &metrics,
            "scheduler.queue.oldest_age",
            &[("client", "client_1")]
        ),
        vec![0.0]
    );
    s.command(Command::Policy {
        policy: Policy::FirstFit,
    })
    .await
    .unwrap();
    let reset = c.read();
    assert!(gauge(&reset, "scheduler.queue.depth", &[("policy", "best_fit")]).is_empty());
    assert_eq!(
        gauge(
            &reset,
            "scheduler.queue.depth",
            &[("policy", "first_fit"), ("client", "client_1")]
        ),
        vec![0.0]
    );
    assert_eq!(count(&reset, "scheduler.jobs", &[]), n);
    drop(s);
    assert!(gauge(&c.read(), "scheduler.node.jobs.running", &[]).is_empty());
}
#[tokio::test]
async fn full_queue_and_oversized_jobs_count_once_and_include_requested_resources() {
    let c = Capture::new();
    let mut s = Session::with_meter(
        42,
        None,
        JevSettings::default(),
        c.provider.meter("scheduler"),
    )
    .unwrap();
    set(&mut s, 0, config(3.0, 16, 30000)).await;
    set(&mut s, 1, config(3.0, 16, 30000)).await;
    set(&mut s, 2, config(3.0, 32, 30000)).await;
    steps(&mut s, 28).await;
    let data = s.data();
    let metrics = c.read();
    let large = data.jobs.iter().filter(|j| j.client == 2).count() as u64;
    let full = data
        .jobs
        .iter()
        .filter(|j| j.client != 2 && j.phase == JobPhase::Rejected)
        .count() as u64;
    assert!(full > 0 && large > 0);
    assert_eq!(
        count(
            &metrics,
            "scheduler.jobs",
            &[("reason", "too_large"), ("error", "true")]
        ),
        large
    );
    assert_eq!(
        count(&metrics, "scheduler.jobs", &[("reason", "queue_full")]),
        full
    );
    assert_eq!(
        histogram(&metrics, "scheduler.job.cpu", &[]).0,
        data.jobs.len() as u64
    );
    assert_eq!(
        histogram(
            &metrics,
            "scheduler.job.duration",
            &[("outcome", "rejected")]
        ),
        (large + full, 0.0)
    );
    assert_eq!(
        histogram(
            &metrics,
            "scheduler.job.run.duration",
            &[("outcome", "rejected")]
        )
        .0,
        0
    );
    let frozen = c.read();
    assert_eq!(
        count(&metrics, "scheduler.jobs", &[]),
        count(&frozen, "scheduler.jobs", &[])
    );
    s.command(Command::RemoveClient { id: 0 }).await.unwrap();
    assert!(
        !gauge(
            &c.read(),
            "scheduler.queue.depth",
            &[("client", "client_1")]
        )
        .is_empty(),
        "Queued work outlives its source client"
    );
}
struct DeferredThenFailed {
    calls: AtomicUsize,
    ready: tokio::sync::Notify,
}
impl Evaluator for DeferredThenFailed {
    fn evaluate(&self, _: Evidence) -> Evaluation<'_> {
        Box::pin(async move {
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            self.ready.notify_one();
            if n == 0 {
                Inference {
                    choice: Some(Choice::Defer),
                    confidence: None,
                    probabilities: Default::default(),
                    model: None,
                    usage: None,
                    latency_ms: 0.0,
                    error: None,
                }
            } else {
                Inference::failed("fixture error")
            }
        })
    }
}
#[tokio::test]
async fn deferral_and_inference_failure_are_placement_outcomes_not_terminal_jobs() {
    let c = Capture::new();
    let judge = Arc::new(DeferredThenFailed {
        calls: AtomicUsize::new(0),
        ready: tokio::sync::Notify::new(),
    });
    let mut s = Session::with_meter(
        42,
        Some(judge.clone()),
        JevSettings {
            max_evaluations: 2,
            dispatch_interval: Duration::ZERO,
            ..Default::default()
        },
        c.provider.meter("scheduler"),
    )
    .unwrap();
    set(&mut s, 0, config(1.0, 1, 1000)).await;
    steps(&mut s, 1).await;
    tokio::time::timeout(Duration::from_secs(1), judge.ready.notified())
        .await
        .unwrap();
    steps(&mut s, 1).await;
    tokio::time::timeout(Duration::from_secs(1), judge.ready.notified())
        .await
        .unwrap();
    steps(&mut s, 1).await;
    let m = c.read();
    assert_eq!(
        count(
            &m,
            "scheduler.placements",
            &[("outcome", "deferred"), ("error", "false")]
        ),
        1
    );
    assert_eq!(
        count(
            &m,
            "scheduler.placements",
            &[("outcome", "evaluation_error"), ("error", "true")]
        ),
        1
    );
    assert_eq!(count(&m, "scheduler.jobs", &[]), 0);
    assert_eq!(histogram(&m, "scheduler.queue.wait", &[]).0, 0);
    assert!(s.data().jobs.iter().all(|j| j.phase == JobPhase::Queued));
}
