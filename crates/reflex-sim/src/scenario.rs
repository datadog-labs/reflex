use crate::Error;
use serde::{Deserialize, Serialize};

/// Times are milliseconds; rates are offered requests per second.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Scenario {
    pub id: String,
    pub name: String,
    pub description: String,
    pub duration_ms: f64,
    pub timeout_ms: f64,
    pub sample_ms: f64,
    pub stress_build_ms: f64,
    pub stress_recovery_ms: f64,
    pub stress_error_probability: f64,
    pub services: Vec<Service>,
    pub phases: Vec<Phase>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Service {
    pub id: String,
    pub name: String,
    pub workers: usize,
    pub queue_limit: usize,
    pub work_ms: f64,
    pub rate: f64,
    pub base_error_probability: f64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Phase {
    pub name: String,
    pub start_ms: f64,
    pub end_ms: f64,
    pub description: String,
    pub changes: Vec<Change>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Change {
    pub service: String,
    pub rate_multiplier: f64,
    pub latency_multiplier: f64,
    pub error_probability: f64,
}
#[derive(Debug, Clone, Copy, Serialize)]
pub struct Environment {
    pub rate: f64,
    pub latency_multiplier: f64,
    pub error_probability: f64,
}
impl Scenario {
    pub fn environment(&self, service: usize, at: f64) -> Environment {
        let s = &self.services[service];
        let change = self
            .phases
            .iter()
            .find(|p| at >= p.start_ms && at < p.end_ms)
            .and_then(|p| p.changes.iter().find(|c| c.service == s.id));
        match change {
            Some(c) => Environment {
                rate: s.rate * c.rate_multiplier,
                latency_multiplier: c.latency_multiplier,
                error_probability: 1.0
                    - (1.0 - s.base_error_probability) * (1.0 - c.error_probability),
            },
            None => Environment {
                rate: s.rate,
                latency_multiplier: 1.0,
                error_probability: s.base_error_probability,
            },
        }
    }
    pub fn validate(&self) -> Result<(), Error> {
        let fail = |s: &str| Error::Invalid(s.into());
        if self.id.is_empty() || self.name.is_empty() {
            return Err(fail("scenario id and name are required"));
        }
        for (label, value) in [
            ("duration", self.duration_ms),
            ("timeout", self.timeout_ms),
            ("sample interval", self.sample_ms),
            ("stress build time", self.stress_build_ms),
            ("stress recovery time", self.stress_recovery_ms),
        ] {
            if !value.is_finite() || value <= 0.0 {
                return Err(Error::Invalid(format!(
                    "{label} must be finite and positive"
                )));
            }
        }
        if self.duration_ms > 600_000.0 || self.timeout_ms > 60_000.0 || self.sample_ms < 50.0 {
            return Err(fail(
                "maximum duration is 600s, timeout 60s, and minimum sample interval 50ms",
            ));
        }
        if !(0.0..=1.0).contains(&self.stress_error_probability) {
            return Err(fail("stress error probability must be in 0..=1"));
        }
        if self.services.is_empty() || self.services.len() > 8 {
            return Err(fail("use 1–8 downstream services"));
        }
        let mut ids = std::collections::HashSet::new();
        for s in &self.services {
            if s.id.is_empty() || !ids.insert(&s.id) {
                return Err(fail("service ids must be nonempty and unique"));
            }
            if s.workers == 0 || s.workers > 128 || s.queue_limit > 1024 {
                return Err(fail("workers must be 1–128 and queue limit at most 1024"));
            }
            if !s.work_ms.is_finite()
                || s.work_ms <= 0.0
                || s.work_ms > 10_000.0
                || !s.rate.is_finite()
                || !(0.0..=500.0).contains(&s.rate)
                || !(0.0..=1.0).contains(&s.base_error_probability)
            {
                return Err(fail("invalid service work, rate, or error probability"));
            }
        }
        let mut end = 0.0;
        for p in &self.phases {
            if p.start_ms != end || !p.end_ms.is_finite() || p.end_ms <= p.start_ms {
                return Err(fail(
                    "phases must be contiguous, ordered, and nonempty, starting at zero",
                ));
            }
            end = p.end_ms;
            let mut changed = std::collections::HashSet::new();
            for c in &p.changes {
                if !ids.contains(&c.service) || !changed.insert(&c.service) {
                    return Err(fail("phase changes must refer to distinct known services"));
                }
                if !c.rate_multiplier.is_finite()
                    || !(0.0..=20.0).contains(&c.rate_multiplier)
                    || !c.latency_multiplier.is_finite()
                    || !(0.1..=100.0).contains(&c.latency_multiplier)
                    || !(0.0..=1.0).contains(&c.error_probability)
                {
                    return Err(fail("invalid phase rate, slowdown, or error probability"));
                }
            }
        }
        if end != self.duration_ms {
            return Err(fail("phases must cover the full scenario duration"));
        }
        Ok(())
    }
}
fn phase(name: &str, start: f64, end: f64, description: &str, changes: Vec<Change>) -> Phase {
    Phase {
        name: name.into(),
        start_ms: start,
        end_ms: end,
        description: description.into(),
        changes,
    }
}
fn change(service: &str, rate: f64, latency: f64, error: f64) -> Change {
    Change {
        service: service.into(),
        rate_multiplier: rate,
        latency_multiplier: latency,
        error_probability: error,
    }
}
pub fn presets() -> Vec<Scenario> {
    let services = vec![
        Service {
            id: "catalog".into(),
            name: "Catalog".into(),
            workers: 4,
            queue_limit: 128,
            work_ms: 85.0,
            rate: 20.0,
            base_error_probability: 0.005,
        },
        Service {
            id: "payments".into(),
            name: "Payments".into(),
            workers: 4,
            queue_limit: 128,
            work_ms: 120.0,
            rate: 14.0,
            base_error_probability: 0.005,
        },
        Service {
            id: "search".into(),
            name: "Search".into(),
            workers: 6,
            queue_limit: 192,
            work_ms: 150.0,
            rate: 18.0,
            base_error_probability: 0.005,
        },
    ];
    let mut scenarios = Vec::new();
    for (id,name,description,changes) in [
        ("healthy","Healthy control","A quiet reference run. Protection should preserve useful traffic without inventing an incident.",vec![]),
        ("slowdown","Slow dependency","Payments becomes six times slower. Requests pile up, time out, and keep consuming downstream capacity.",vec![change("payments",1.0,6.0,0.03)]),
        ("error-storm","Error storm","Search returns errors on 85% of completions. Failed work still occupies its workers.",vec![change("search",1.0,1.5,0.85)]),
        ("traffic-surge","Traffic surge","All three services receive four times their usual traffic. Queue pressure creates errors even without an injected outage.",vec![change("catalog",4.0,1.0,0.0),change("payments",4.0,1.0,0.0),change("search",4.0,1.0,0.0)]),
        ("brief-spike","Brief spike","A two-second slowdown tests whether a breaker overreacts to a disturbance that would clear on its own.",vec![change("payments",1.5,4.0,0.1)]),
        ("flapping","Flapping dependency","Payments repeatedly fails and recovers. Recovery probes can land on either side of the next fault.",vec![]),
    ] {
        let end = if id == "brief-spike" {22_000.0} else {45_000.0};
        let mut phases = vec![phase("Steady traffic",0.0,20_000.0,"Observe the healthy system before changing its environment.",vec![])];
        if id == "flapping" {
            for i in 0..6 { let start = 20_000.0 + i as f64*5_000.0; let faulty = i%2==0;
                phases.push(phase(if faulty {"Fault on"} else {"Fault off"}, start,start+5_000.0,
                    if faulty {"Payments: 75% injected errors and 3× service time."} else {"The injected fault clears; accumulated stress can persist."},
                    if faulty {vec![change("payments",1.0,3.0,0.75)]} else {vec![]})); }
            phases.push(phase("Recovery",50_000.0,90_000.0,"Return to normal traffic and let remaining work drain.",vec![]));
        } else {
            phases.push(phase(if id=="healthy" {"Control window"} else {"Disturbance"},20_000.0,end,description,changes));
            phases.push(phase("Recovery",end,65_000.0,"Restore the environment. Queued and timed-out requests still have to finish.",vec![]));
            phases.push(phase("Steady again",65_000.0,90_000.0,"Observe recovery under the original offered traffic.",vec![]));
        }
        scenarios.push(Scenario { id:id.into(), name:name.into(), description:description.into(), duration_ms:90_000.0,timeout_ms:650.0,
            sample_ms:250.0,stress_build_ms:1_500.0,stress_recovery_ms:5_000.0,stress_error_probability:0.8,services:services.clone(),phases });
    }
    scenarios
}
