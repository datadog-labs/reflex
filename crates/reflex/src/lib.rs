// Unless explicitly stated otherwise all files in this repository are licensed under the Apache-2.0 License.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-present Datadog, Inc.

//! Typed heuristic decisions and deterministic, transactional execution.
//!
//! See `examples/circuit_breaker.rs` and the workspace SDK guide.
//! Runtime guarantees apply to isolated candidate data in the in-memory store.
//! Hooks must be pure, short, synchronous operations; effects own external I/O.

mod controller;
mod machine;
mod telemetry;
pub use controller::*;
pub use machine::*;
pub use reflex_macros::state_machine;
