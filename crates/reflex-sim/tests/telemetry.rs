// Unless explicitly stated otherwise all files in this repository are licensed under the Apache-2.0 License.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-present Datadog, Inc.

use opentelemetry::{metrics::MeterProvider, KeyValue};
use opentelemetry_sdk::metrics::{
    data::{AggregatedMetrics, MetricData, ResourceMetrics},
    InMemoryMetricExporter, PeriodicReader, SdkMeterProvider,
};
use reflex_sim::{
    engine::{
        live::{Faults, LiveSimulation},
        simulate_with_meter,
    },
    jev::PolicyKind,
    policy::{builtins, PolicyFactory, ThresholdConfig, ThresholdPolicy},
    presets,
    scenario::{Change, Phase, Scenario},
    trace::{Request, Trace},
};
use std::time::Duration;

struct Capture {
    provider: SdkMeterProvider,
    exporter: InMemoryMetricExporter,
}
impl Capture {
    fn new() -> Self {
        let exporter = InMemoryMetricExporter::default();
        let provider = SdkMeterProvider::builder()
            .with_reader(
                PeriodicReader::builder(exporter.clone())
                    .with_interval(Duration::from_secs(3600))
                    .build(),
            )
            .build();
        Self { provider, exporter }
    }
    fn read(&self) -> ResourceMetrics {
        self.provider.force_flush().unwrap();
        self.exporter.get_finished_metrics().unwrap().pop().unwrap()
    }
}
impl Drop for Capture {
    fn drop(&mut self) {
        self.provider.shutdown().unwrap();
    }
}
fn matches(attributes: impl Iterator<Item = KeyValue>, filter: &[(&str, &str)]) -> bool {
    let attributes: Vec<_> = attributes.collect();
    filter.iter().all(|(key, value)| {
        attributes
            .iter()
            .any(|a| a.key.as_str() == *key && a.value.to_string() == *value)
    })
}
fn count(metrics: &ResourceMetrics, name: &str, filter: &[(&str, &str)]) -> u64 {
    metrics
        .scope_metrics()
        .flat_map(|s| s.metrics())
        .filter(|m| m.name() == name)
        .map(|m| match m.data() {
            AggregatedMetrics::U64(MetricData::Sum(sum)) => sum
                .data_points()
                .filter(|p| matches(p.attributes().cloned(), filter))
                .map(|p| p.value())
                .sum::<u64>(),
            _ => panic!("Expected counter: {name}"),
        })
        .sum()
}
fn histogram(metrics: &ResourceMetrics, name: &str, filter: &[(&str, &str)]) -> (u64, f64) {
    metrics
        .scope_metrics()
        .flat_map(|s| s.metrics())
        .filter(|m| m.name() == name)
        .fold((0, 0.0), |(count, sum), m| match m.data() {
            AggregatedMetrics::F64(MetricData::Histogram(h)) => h
                .data_points()
                .filter(|p| matches(p.attributes().cloned(), filter))
                .fold((count, sum), |(count, sum), p| {
                    (count + p.count(), sum + p.sum())
                }),
            _ => panic!("Expected histogram: {name}"),
        })
}
fn gauge(metrics: &ResourceMetrics, name: &str, filter: &[(&str, &str)]) -> Vec<f64> {
    metrics
        .scope_metrics()
        .flat_map(|s| s.metrics())
        .filter(|m| m.name() == name)
        .flat_map(|m| match m.data() {
            AggregatedMetrics::U64(MetricData::Gauge(g)) => g
                .data_points()
                .filter(|p| matches(p.attributes().cloned(), filter))
                .map(|p| p.value() as f64)
                .collect::<Vec<_>>(),
            AggregatedMetrics::F64(MetricData::Gauge(g)) => g
                .data_points()
                .filter(|p| matches(p.attributes().cloned(), filter))
                .map(|p| p.value())
                .collect(),
            _ => panic!("Expected gauge: {name}"),
        })
        .collect()
}
fn tiny() -> Scenario {
    let mut s = presets().remove(0);
    s.services.truncate(1);
    s.services[0].workers = 1;
    s.services[0].queue_limit = 1;
    s.services[0].rate = 0.0;
    s.services[0].base_error_probability = 0.0;
    s.duration_ms = 1000.0;
    s.timeout_ms = 500.0;
    s.sample_ms = 50.0;
    s.stress_error_probability = 0.0;
    s.phases = vec![Phase {
        name: "Control".into(),
        start_ms: 0.0,
        end_ms: 1000.0,
        description: String::new(),
        changes: vec![],
    }];
    s
}
fn trace(s: &Scenario, arrivals: &[(f64, f64)]) -> Trace {
    Trace {
        model_version: "queue-stress-v1".into(),
        scenario: s.id.clone(),
        seed: 42,
        fingerprint: "fixture".into(),
        requests: arrivals
            .iter()
            .enumerate()
            .map(|(id, (at, work))| Request {
                id,
                at_ms: *at,
                work_ms: *work,
                service: 0,
                error_roll: 0.5,
            })
            .collect(),
    }
}
#[tokio::test]
async fn client_timeout_and_late_http_success_are_counted_independently_once() {
    let c = Capture::new();
    let mut s = tiny();
    s.timeout_ms = 50.0;
    let run = simulate_with_meter(
        &s,
        &trace(&s, &[(0.0, 100.0)]),
        builtins()[0],
        c.provider.meter("traffic"),
    )
    .await
    .unwrap();
    assert_eq!(run.requests[0].http_status_code, Some(200));
    let metrics = c.read();
    assert_eq!(count(&metrics, "http.client.requests", &[]), 1);
    assert_eq!(
        count(
            &metrics,
            "http.client.requests",
            &[
                ("upstream", "catalog"),
                ("outcome", "timeout"),
                ("error", "true")
            ]
        ),
        1
    );
    assert_eq!(
        count(
            &metrics,
            "http.client.requests",
            &[("http.status_code", "200")]
        ),
        0
    );
    assert_eq!(
        count(
            &metrics,
            "http.server.requests",
            &[
                ("service", "catalog"),
                ("error", "false"),
                ("http.status_code", "200")
            ]
        ),
        1
    );
    assert_eq!(
        histogram(&metrics, "http.client.request.duration", &[]),
        (1, 0.05)
    );
    assert_eq!(
        histogram(&metrics, "http.server.request.duration", &[]),
        (1, 0.1)
    );
}
#[tokio::test]
async fn queue_rejection_is_an_http_503_and_queue_wait_is_recorded_at_worker_start() {
    let c = Capture::new();
    let s = tiny();
    let run = simulate_with_meter(
        &s,
        &trace(&s, &[(0.0, 100.0), (10.0, 100.0), (20.0, 100.0)]),
        builtins()[0],
        c.provider.meter("traffic"),
    )
    .await
    .unwrap();
    assert_eq!(run.requests[2].http_status_code, Some(503));
    let metrics = c.read();
    assert_eq!(count(&metrics, "http.client.requests", &[]), 3);
    assert_eq!(count(&metrics, "http.server.requests", &[]), 3);
    for name in ["http.client.requests", "http.server.requests"] {
        assert_eq!(
            count(
                &metrics,
                name,
                &[("error", "true"), ("http.status_code", "503")]
            ),
            1
        );
    }
    assert_eq!(
        count(
            &metrics,
            "http.client.requests",
            &[("outcome", "circuit_open")]
        ),
        0
    );
    assert_eq!(
        histogram(&metrics, "http.server.queue.wait", &[]),
        (2, 0.09)
    );
    let (_, duration) = histogram(&metrics, "http.server.request.duration", &[]);
    assert!((duration - 0.29).abs() < 1e-10);
}
#[tokio::test]
async fn breaker_shedding_and_successful_probe_have_distinct_metrics() {
    let c = Capture::new();
    let mut s = tiny();
    s.phases = vec![
        Phase {
            end_ms: 150.0,
            changes: vec![Change {
                service: "catalog".into(),
                rate_multiplier: 1.0,
                latency_multiplier: 1.0,
                error_probability: 1.0,
            }],
            ..s.phases[0].clone()
        },
        Phase {
            start_ms: 150.0,
            ..s.phases[0].clone()
        },
    ];
    let factory = PolicyFactory {
        id: "threshold",
        name: "Threshold",
        description: "fixture",
        create: || {
            Ok(Box::new(ThresholdPolicy::new(ThresholdConfig {
                window_ms: 1000.0,
                minimum_samples: 1,
                failure_ratio: 0.5,
                cooldown_ms: 100.0,
            })?))
        },
    };
    let run = simulate_with_meter(
        &s,
        &trace(&s, &[(0.0, 100.0), (110.0, 100.0), (210.0, 100.0)]),
        factory,
        c.provider.meter("traffic"),
    )
    .await
    .unwrap();
    assert!(run.requests[2].probe);
    let metrics = c.read();
    assert_eq!(count(&metrics, "http.client.requests", &[]), 3);
    assert_eq!(count(&metrics, "http.server.requests", &[]), 2);
    assert_eq!(
        count(
            &metrics,
            "http.client.requests",
            &[("outcome", "circuit_open"), ("error", "true")]
        ),
        1
    );
    assert_eq!(
        count(
            &metrics,
            "http.server.requests",
            &[("http.status_code", "500")]
        ),
        1
    );
    assert_eq!(
        count(
            &metrics,
            "circuit_breaker.probes",
            &[("outcome", "success")]
        ),
        1
    );
    assert_eq!(
        count(
            &metrics,
            "circuit_breaker.transitions",
            &[("from", "closed"), ("to", "open")]
        ),
        1
    );
    assert_eq!(
        count(
            &metrics,
            "circuit_breaker.transitions",
            &[("from", "open"), ("to", "probe")]
        ),
        1
    );
    assert_eq!(
        count(
            &metrics,
            "circuit_breaker.transitions",
            &[("from", "probe"), ("to", "closed")]
        ),
        1
    );
}
#[tokio::test]
async fn gauges_match_all_three_services_and_remain_available_while_paused() {
    let c = Capture::new();
    let mut live = LiveSimulation::with_policy_and_meter(
        42,
        PolicyKind::Threshold,
        c.provider.meter("traffic"),
    )
    .unwrap();
    live.set_faults(
        1,
        Faults {
            slow: true,
            errors: true,
            surge: true,
        },
    )
    .unwrap();
    live.advance_to(12000.0).await.unwrap();
    let view = live.view();
    for metrics in [c.read(), c.read()] {
        for service in &view.services {
            let server = [("service", service.definition.id.as_str())];
            let client = [("upstream", service.definition.id.as_str())];
            assert_eq!(
                gauge(&metrics, "http.server.queue.depth", &server),
                vec![service.state.queued as f64]
            );
            assert_eq!(
                gauge(&metrics, "http.server.active", &server),
                vec![service.state.active as f64]
            );
            let timed_out_active = live
                .export()
                .requests
                .iter()
                .filter(|r| {
                    r.service == service.state.service
                        && r.started_ms.is_some()
                        && r.downstream_finished_ms.is_none()
                        && r.outcome == Some(reflex_sim::policy::ClientOutcome::Timeout)
                })
                .count();
            assert_eq!(
                gauge(&metrics, "http.server.timed_out_work", &server),
                vec![timed_out_active as f64]
            );
            assert_eq!(
                gauge(&metrics, "http.server.utilization", &server),
                vec![service.state.active as f64 / service.definition.workers as f64]
            );
            let counts = &service.state.counts;
            assert_eq!(
                gauge(&metrics, "http.client.in_flight", &client),
                vec![
                    (counts.offered - counts.success - counts.error - counts.timeout - counts.shed)
                        as f64
                ]
            );
            assert_eq!(
                gauge(&metrics, "circuit_breaker.state", &client)
                    .iter()
                    .sum::<f64>(),
                1.0
            );
            assert_eq!(
                gauge(
                    &metrics,
                    "circuit_breaker.state",
                    &[
                        ("upstream", &service.definition.id),
                        (
                            "state",
                            match service.state.phase {
                                reflex_sim::policy::CircuitPhase::Closed => "closed",
                                reflex_sim::policy::CircuitPhase::Open => "open",
                                _ => "probe",
                            }
                        )
                    ]
                ),
                vec![1.0]
            );
        }
    }
    // A stopped incident must not keep publishing a phantom queue or breaker state.
    drop(live);
    let metrics = c.read();
    assert!(gauge(&metrics, "http.server.active", &[]).is_empty());
    assert!(gauge(&metrics, "circuit_breaker.state", &[]).is_empty());
}
#[tokio::test]
async fn replacing_an_incident_does_not_leave_stale_gauge_observations() {
    let c = Capture::new();
    let mut live = LiveSimulation::with_policy_and_meter(
        42,
        PolicyKind::Threshold,
        c.provider.meter("traffic"),
    )
    .unwrap();
    live.advance_to(1000.0).await.unwrap();
    drop(live);
    let next = LiveSimulation::with_policy_and_meter(
        42,
        PolicyKind::Threshold,
        c.provider.meter("traffic"),
    )
    .unwrap();
    let metrics = c.read();
    assert_eq!(
        gauge(
            &metrics,
            "http.client.in_flight",
            &[("upstream", "catalog")]
        ),
        vec![0.0]
    );
    assert_eq!(
        gauge(
            &metrics,
            "circuit_breaker.state",
            &[("upstream", "catalog"), ("state", "closed")]
        ),
        vec![1.0]
    );
    let allowed = [
        "http.client.requests",
        "http.client.request.duration",
        "http.client.in_flight",
        "http.server.requests",
        "http.server.request.duration",
        "http.server.queue.depth",
        "http.server.queue.wait",
        "http.server.active",
        "http.server.utilization",
        "http.server.timed_out_work",
        "circuit_breaker.state",
        "circuit_breaker.transitions",
        "circuit_breaker.probes",
    ];
    for m in metrics.scope_metrics().flat_map(|s| s.metrics()) {
        assert!(allowed.contains(&m.name()));
    }
    drop(next);
}

#[tokio::test]
async fn timed_out_probe_is_not_counted_again_when_its_server_work_succeeds() {
    let c = Capture::new();
    let mut s = tiny();
    s.phases = vec![
        Phase {
            end_ms: 150.0,
            changes: vec![Change {
                service: "catalog".into(),
                rate_multiplier: 1.0,
                latency_multiplier: 1.0,
                error_probability: 1.0,
            }],
            ..s.phases[0].clone()
        },
        Phase {
            start_ms: 150.0,
            ..s.phases[0].clone()
        },
    ];
    let factory = PolicyFactory {
        id: "threshold",
        name: "Threshold",
        description: "fixture",
        create: || {
            Ok(Box::new(ThresholdPolicy::new(ThresholdConfig {
                window_ms: 1000.0,
                minimum_samples: 1,
                failure_ratio: 0.5,
                cooldown_ms: 100.0,
            })?))
        },
    };
    simulate_with_meter(
        &s,
        &trace(&s, &[(0.0, 100.0), (210.0, 600.0)]),
        factory,
        c.provider.meter("traffic"),
    )
    .await
    .unwrap();
    let metrics = c.read();
    assert_eq!(count(&metrics, "circuit_breaker.probes", &[]), 1);
    assert_eq!(
        count(
            &metrics,
            "circuit_breaker.probes",
            &[("outcome", "timeout")]
        ),
        1
    );
    assert_eq!(
        count(
            &metrics,
            "http.server.requests",
            &[("http.status_code", "200")]
        ),
        1
    );
    assert_eq!(
        count(&metrics, "http.client.requests", &[("outcome", "success")]),
        0
    );
}
