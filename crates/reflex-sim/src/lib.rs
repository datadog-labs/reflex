//! Discrete-event circuit-breaking laboratory with interchangeable client policies.
//! Simulated time is independent of wall-clock time. The same immutable traffic
//! trace and random draws can be replayed against every policy.
pub mod capacity;
mod decision_trace;
pub mod engine;
pub mod jev;
pub mod playground;
pub mod policy;
pub mod recovery;
pub mod report;
pub mod scenario;
pub mod scheduler;
mod telemetry;
pub mod trace;

pub use engine::{simulate, simulate_with_meter, Run};
pub use policy::{Policy, PolicyFactory};
pub use scenario::{presets, Scenario};
pub use trace::{generate_trace, Trace};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid simulation: {0}")]
    Invalid(String),
    #[error("policy failed: {0}")]
    Policy(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

pub mod datadog;

#[cfg(test)]
#[allow(dead_code)]
#[path = "../tests/support/metrics.rs"]
mod metric_capture;

pub mod forecasting;
