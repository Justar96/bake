//! Attempt accounting observes the request path without adding a consumer or retry.

use std::sync::atomic::{AtomicUsize, Ordering};

use bake_agent::{RunControl, RunLimits};

use super::*;

#[tokio::test]
async fn local_cancel_during_debit_completion_prevents_dispatch_in_the_same_poll() {
    let controller = bake_ai::AbortController::new();
    let cancelling = controller.clone();
    let observer: bake_agent::AdmissionObserver = Arc::new(move |_| {
        let cancelling = cancelling.clone();
        Box::pin(async move {
            // Complete the required write and cancel within the request's
            // poll, before select can forward the signal to its controller.
            cancelling.abort();
            Ok(())
        })
    });
    let control = RunControl::with_admission_observer(RunLimits::default(), Some(observer));
    let calls = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&calls);
    let stream_fn: StreamFn = Arc::new(move |_, _, _| {
        counted.fetch_add(1, Ordering::SeqCst);
        Err("a locally cancelled request must not be dispatched".into())
    });
    let attempts = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&attempts);
    let options = SummarizationOptions {
        run_control: Some(control.clone()),
        foreground_call_reserve: 1,
        signal: Some(controller.signal()),
        on_attempt: Some(Arc::new(move |attempt| {
            recorded.lock().unwrap().push(attempt.clone())
        })),
        ..key()
    };
    let result = generate_summary(
        &summarize_this(),
        &test_model(false, 8192),
        None,
        None,
        &stream_fn,
        &options,
    )
    .await;
    assert!(result.is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(control.model_calls(), 1);
    assert_eq!(control.stopped(), None);
    let attempts = attempts.lock().unwrap();
    assert_eq!(attempts.len(), 1);
    assert!(!attempts[0].dispatched);
    assert_eq!(attempts[0].usage, None);
}

#[tokio::test]
async fn retries_retain_attempt_usage_and_identity_when_the_next_dispatch_fails() {
    let calls = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&calls);
    let stream_fn: StreamFn = Arc::new(move |_, _, _| {
        if counted.fetch_add(1, Ordering::SeqCst) != 0 {
            return Err("dispatch failed without usage".into());
        }
        let mut response = summary_response();
        response.stop_reason = StopReason::Error;
        response.error_message = Some("overloaded_error".into());
        response.usage = Usage {
            input: 13,
            output: 7,
            total_tokens: 20,
            ..Usage::default()
        };
        let (sender, stream) = assistant_message_channel();
        sender.finish(response);
        Ok(stream)
    });
    let attempts = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&attempts);
    let control = RunControl::default();
    let options = SummarizationOptions {
        run_control: Some(control.clone()),
        retry: Some(RetryPolicy {
            enabled: true,
            max_retries: 1,
            base_delay_ms: 0,
            max_agent_delay_ms: None,
        }),
        on_attempt: Some(Arc::new(move |attempt| {
            recorded.lock().unwrap().push(attempt.clone());
            panic!("an observer cannot change request execution");
        })),
        ..key()
    };
    let result = generate_summary(
        &summarize_this(),
        &test_model(false, 8192),
        None,
        None,
        &stream_fn,
        &options,
    )
    .await;
    assert_eq!(result, Err("dispatch failed without usage".into()));
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(control.model_calls(), 2);
    let attempts = attempts.lock().unwrap();
    assert_eq!(attempts.len(), 2);
    assert_eq!(attempts[0].admission.as_ref().unwrap().ordinal, 1);
    assert_eq!(attempts[1].admission.as_ref().unwrap().ordinal, 2);
    assert_eq!(attempts[0].usage.as_ref().unwrap().total_tokens, 20);
    assert_eq!(attempts[0].stop_reason, Some(StopReason::Error));
    assert!(attempts[0].dispatched && attempts[1].dispatched);
    assert_eq!(attempts[1].usage, None);
    assert_eq!(attempts[1].stop_reason, None);
}

#[tokio::test]
async fn partial_split_usage_survives_reserved_allowance_refusal() {
    let mock = Mock::new();
    let control = RunControl::new(RunLimits {
        max_model_calls: Some(2),
        ..RunLimits::default()
    });
    let attempts = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&attempts);
    let options = SummarizationOptions {
        run_control: Some(control.clone()),
        foreground_call_reserve: 1,
        on_attempt: Some(Arc::new(move |attempt| {
            recorded.lock().unwrap().push(attempt.clone())
        })),
        ..key()
    };
    let result = compact(
        &split_preparation(None, 2000, true),
        &test_model(false, 8192),
        None,
        &mock.stream_fn(),
        &options,
    )
    .await;
    assert!(result.unwrap_err().contains("foreground"));
    assert_eq!(mock.options().len(), 1);
    assert_eq!(control.model_calls(), 1);
    assert_eq!(control.stopped(), None);
    let attempts = attempts.lock().unwrap();
    assert_eq!(attempts.len(), 1);
    assert_eq!(attempts[0].usage, Some(summary_response().usage));
}

#[tokio::test]
async fn absent_provider_usage_is_not_reported_as_a_zero_cost_attempt() {
    let stream_fn: StreamFn = Arc::new(|_, _, _| {
        let mut response = summary_response();
        response.usage = Usage::default();
        let (sender, stream) = assistant_message_channel();
        sender.finish(response);
        Ok(stream)
    });
    let attempts = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&attempts);
    let options = SummarizationOptions {
        on_attempt: Some(Arc::new(move |attempt| {
            recorded.lock().unwrap().push(attempt.clone())
        })),
        ..key()
    };
    generate_summary(
        &summarize_this(),
        &test_model(false, 8192),
        None,
        None,
        &stream_fn,
        &options,
    )
    .await
    .unwrap();
    let attempts = attempts.lock().unwrap();
    assert_eq!(attempts.len(), 1);
    assert!(attempts[0].dispatched);
    assert_eq!(attempts[0].usage, None);
}
