// Unless explicitly stated otherwise all files in this repository are licensed under the Apache-2.0 License.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-present Datadog, Inc.

use opentelemetry::KeyValue;
use opentelemetry_sdk::metrics::{
    data::{AggregatedMetrics, MetricData, ResourceMetrics},
    InMemoryMetricExporter, PeriodicReader, SdkMeterProvider,
};
use std::time::Duration;
pub(crate) struct Capture {
    pub(crate) provider: SdkMeterProvider,
    exporter: InMemoryMetricExporter,
}
impl Capture {
    pub(crate) fn new() -> Self {
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
    pub(crate) fn read(&self) -> ResourceMetrics {
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
pub(crate) fn count(metrics: &ResourceMetrics, name: &str, filter: &[(&str, &str)]) -> u64 {
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
pub(crate) fn histogram(
    metrics: &ResourceMetrics,
    name: &str,
    filter: &[(&str, &str)],
) -> (u64, f64) {
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
            AggregatedMetrics::U64(MetricData::Histogram(h)) => h
                .data_points()
                .filter(|p| matches(p.attributes().cloned(), filter))
                .fold((count, sum), |(count, sum), p| {
                    (count + p.count(), sum + p.sum() as f64)
                }),
            _ => panic!("Expected histogram: {name}"),
        })
}
pub(crate) fn gauge(metrics: &ResourceMetrics, name: &str, filter: &[(&str, &str)]) -> Vec<f64> {
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
