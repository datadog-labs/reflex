// Unless explicitly stated otherwise all files in this repository are licensed under the Apache-2.0 License.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-present Datadog, Inc.

//! Circuit-breaker traffic telemetry. No exporter or subscriber is installed here.
use crate::policy::{CircuitPhase, ClientOutcome};
use opentelemetry::{
    metrics::{Counter, Histogram, Meter},
    KeyValue,
};
use std::sync::{Arc, Mutex};

#[derive(Clone)]
pub(crate) struct Gauges {
    pub client_in_flight: u64,
    pub queued: u64,
    pub active: u64,
    pub utilization: f64,
    pub timed_out_work: u64,
    pub phase: CircuitPhase,
}
impl Default for Gauges {
    fn default() -> Self {
        Self {
            client_in_flight: 0,
            queued: 0,
            active: 0,
            utilization: 0.0,
            timed_out_work: 0,
            phase: CircuitPhase::Closed,
        }
    }
}
struct ServiceGauges {
    client: Vec<KeyValue>,
    server: Vec<KeyValue>,
    values: Gauges,
}
pub(crate) struct Telemetry {
    gauges: Arc<Mutex<Vec<ServiceGauges>>>,
    client: Vec<Vec<KeyValue>>,
    server: Vec<Vec<KeyValue>>,
    client_requests: Counter<u64>,
    client_duration: Histogram<f64>,
    server_requests: Counter<u64>,
    server_duration: Histogram<f64>,
    queue_wait: Histogram<f64>,
    transitions: Counter<u64>,
    probes: Counter<u64>,
}
pub(crate) fn phase(phase: CircuitPhase) -> &'static str {
    match phase {
        CircuitPhase::Bypassed => "bypassed",
        CircuitPhase::Closed => "closed",
        CircuitPhase::Open => "open",
        CircuitPhase::HalfOpen => "probe",
    }
}
impl Telemetry {
    pub fn new(meter: Meter, services: &[crate::scenario::Service], policy: &'static str) -> Self {
        Self::with_run(meter, services, policy, None)
    }
    pub fn with_run(
        meter: Meter,
        services: &[crate::scenario::Service],
        policy: &'static str,
        run: Option<&str>,
    ) -> Self {
        let mut client: Vec<_> = services
            .iter()
            .map(|s| {
                vec![
                    KeyValue::new("upstream", s.id.clone()),
                    KeyValue::new("policy", policy),
                ]
            })
            .collect();
        let mut server: Vec<_> = services
            .iter()
            .map(|s| {
                vec![
                    KeyValue::new("service", s.id.clone()),
                    KeyValue::new("policy", policy),
                ]
            })
            .collect();
        if let Some(run) = run {
            for tags in client.iter_mut().chain(server.iter_mut()) {
                tags.push(KeyValue::new("simulation_run", run.to_owned()));
            }
        }
        let gauges = Arc::new(Mutex::new(
            services
                .iter()
                .enumerate()
                .map(|(i, _)| ServiceGauges {
                    client: client[i].clone(),
                    server: server[i].clone(),
                    values: Gauges::default(),
                })
                .collect::<Vec<_>>(),
        ));
        // Weak callbacks expire when an incident ends. Resetting cannot leave old gauge values
        // competing with the replacement incident, and callbacks never lock the simulation.
        for (name, description, read, client_side) in [
            (
                "http.client.in_flight",
                "Outstanding client attempts",
                (|g: &Gauges| g.client_in_flight) as fn(&Gauges) -> u64,
                true,
            ),
            (
                "http.server.queue.depth",
                "Requests waiting for a worker",
                |g: &Gauges| g.queued,
                false,
            ),
            (
                "http.server.active",
                "Requests occupying workers",
                |g: &Gauges| g.active,
                false,
            ),
            (
                "http.server.timed_out_work",
                "Active server work whose client has timed out",
                |g: &Gauges| g.timed_out_work,
                false,
            ),
        ] {
            let weak = Arc::downgrade(&gauges);
            meter
                .u64_observable_gauge(name)
                .with_description(description)
                .with_unit("{request}")
                .with_callback(move |observer| {
                    if let Some(gauges) = weak.upgrade() {
                        let values = gauges.lock().unwrap_or_else(|e| e.into_inner());
                        for service in values.iter() {
                            observer.observe(
                                read(&service.values),
                                if client_side {
                                    &service.client
                                } else {
                                    &service.server
                                },
                            );
                        }
                    }
                })
                .build();
        }
        let weak = Arc::downgrade(&gauges);
        meter
            .f64_observable_gauge("http.server.utilization")
            .with_description("Fraction of worker capacity occupied")
            .with_unit("1")
            .with_callback(move |observer| {
                if let Some(gauges) = weak.upgrade() {
                    for service in gauges.lock().unwrap_or_else(|e| e.into_inner()).iter() {
                        observer.observe(service.values.utilization, &service.server);
                    }
                }
            })
            .build();
        let weak = Arc::downgrade(&gauges);
        meter
            .u64_observable_gauge("circuit_breaker.state")
            .with_description("One for the current state, zero for the other states")
            .with_unit("1")
            .with_callback(move |observer| {
                if let Some(gauges) = weak.upgrade() {
                    for service in gauges.lock().unwrap_or_else(|e| e.into_inner()).iter() {
                        if service.values.phase == CircuitPhase::Bypassed {
                            continue;
                        }
                        for state in ["closed", "open", "probe"] {
                            let mut attributes = service.client.clone();
                            attributes.push(KeyValue::new("state", state));
                            observer.observe(
                                u64::from(phase(service.values.phase) == state),
                                &attributes,
                            );
                        }
                    }
                }
            })
            .build();
        Self {
            gauges,
            client,
            server,
            client_requests: meter
                .u64_counter("http.client.requests")
                .with_description("Client attempts recorded once at terminal outcome")
                .with_unit("{request}")
                .build(),
            client_duration: duration(
                &meter,
                "http.client.request.duration",
                "Client-observed duration in simulated seconds",
            ),
            server_requests: meter
                .u64_counter("http.server.requests")
                .with_description(
                    "Server requests recorded once at completion, including queue rejection",
                )
                .with_unit("{request}")
                .build(),
            server_duration: duration(
                &meter,
                "http.server.request.duration",
                "Server duration including queue wait, in simulated seconds",
            ),
            queue_wait: duration(
                &meter,
                "http.server.queue.wait",
                "Queue wait at worker start, in simulated seconds",
            ),
            transitions: meter
                .u64_counter("circuit_breaker.transitions")
                .with_unit("{transition}")
                .build(),
            probes: meter
                .u64_counter("circuit_breaker.probes")
                .with_description("Recovery probes recorded once at their client outcome")
                .with_unit("{probe}")
                .build(),
        }
    }
    pub fn snapshot(&self, values: impl Iterator<Item = Gauges>) {
        for (service, values) in self
            .gauges
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter_mut()
            .zip(values)
        {
            service.values = values;
        }
    }
    pub fn client_finished(
        &self,
        service: usize,
        outcome: ClientOutcome,
        status: Option<u16>,
        duration_ms: f64,
        probe: bool,
    ) {
        let mut attributes = self.client[service].clone();
        attributes.extend([
            KeyValue::new("http.method", "GET"),
            KeyValue::new("error", outcome != ClientOutcome::Success),
            KeyValue::new(
                "outcome",
                match outcome {
                    ClientOutcome::Success => "success",
                    ClientOutcome::Error => "http_error",
                    ClientOutcome::Timeout => "timeout",
                    ClientOutcome::Shed => "circuit_open",
                },
            ),
        ]);
        // No HTTP response exists for a timeout or locally blocked request.
        if matches!(outcome, ClientOutcome::Success | ClientOutcome::Error) {
            if let Some(status) = status {
                attributes.push(KeyValue::new("http.status_code", i64::from(status)));
            }
        }
        self.client_requests.add(1, &attributes);
        self.client_duration
            .record(duration_ms / 1000.0, &attributes);
        if probe {
            let mut attributes = self.client[service].clone();
            attributes.push(KeyValue::new(
                "outcome",
                match outcome {
                    ClientOutcome::Success => "success",
                    ClientOutcome::Timeout => "timeout",
                    _ => "error",
                },
            ));
            self.probes.add(1, &attributes);
        }
    }
    pub fn server_finished(&self, service: usize, status: u16, duration_ms: f64) {
        let mut attributes = self.server[service].clone();
        attributes.extend([
            KeyValue::new("http.method", "GET"),
            KeyValue::new("error", status >= 400),
            KeyValue::new("http.status_code", i64::from(status)),
        ]);
        self.server_requests.add(1, &attributes);
        self.server_duration
            .record(duration_ms / 1000.0, &attributes);
    }
    pub fn started(&self, service: usize, wait_ms: f64) {
        self.queue_wait
            .record(wait_ms / 1000.0, &self.server[service]);
    }
    pub fn transitioned(&self, service: usize, from: CircuitPhase, to: CircuitPhase) {
        let mut attributes = self.client[service].clone();
        attributes.extend([
            KeyValue::new("from", phase(from)),
            KeyValue::new("to", phase(to)),
        ]);
        self.transitions.add(1, &attributes);
    }
}

fn duration(meter: &Meter, name: &'static str, description: &'static str) -> Histogram<f64> {
    meter
        .f64_histogram(name)
        .with_description(description)
        .with_unit("s")
        // Explicit buckets retain subsecond latency and the 650ms client deadline.
        .with_boundaries(vec![
            0.0, 0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.2, 0.3, 0.5, 0.65, 0.75, 1.0, 2.0, 3.0,
            5.0, 10.0, 30.0, 60.0,
        ])
        .build()
}
