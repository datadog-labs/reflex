// Unless explicitly stated otherwise all files in this repository are licensed under the Apache-2.0 License.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-present Datadog, Inc.

use reflex::*;
use std::time::Duration;
struct Fixed(Option<f64>);
impl Judge<(), u8> for Fixed {
    async fn judge(&self, _: &()) -> Result<Judgment<u8>, JudgeError> {
        Ok(Judgment {
            action: 42,
            confidence: self.0,
        })
    }
}
#[tokio::test]
async fn validates_confidence_without_inventing_missing_values() {
    for value in [None, Some(0.0), Some(1.0), Some(0.7)] {
        let c = Controller::builder().judge(Fixed(value)).build().unwrap();
        let p = c.evaluate(&()).await.unwrap();
        assert_eq!(p.confidence(), value);
        assert_eq!(*p.action(), 42);
    }
    for value in [f64::NAN, f64::INFINITY, -0.1, 1.1] {
        let c = Controller::builder()
            .judge(Fixed(Some(value)))
            .build()
            .unwrap();
        assert!(matches!(
            c.evaluate(&()).await,
            Err(EvaluationError::InvalidConfidence)
        ));
    }
}
struct Never;
impl Judge<(), ()> for Never {
    async fn judge(&self, _: &()) -> Result<Judgment<()>, JudgeError> {
        std::future::pending().await
    }
}
#[tokio::test(start_paused = true)]
async fn whole_judge_operation_is_bounded() {
    let c = Controller::builder()
        .judge(Never)
        .inference_timeout(Duration::from_millis(5))
        .build()
        .unwrap();
    assert!(matches!(
        c.evaluate(&()).await,
        Err(EvaluationError::Timeout)
    ));
}
#[test]
fn rejects_zero_deadline() {
    assert!(Controller::builder()
        .judge(Never)
        .inference_timeout(Duration::ZERO)
        .build()
        .is_err());
}
