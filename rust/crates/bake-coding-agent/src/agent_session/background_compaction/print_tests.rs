//! Print owns the last speculative request through its terminal accounting.

use super::*;
use crate::print_mode::{PrintModeOptions, PrintOutput, SharedWriter, run_print_mode};

#[tokio::test(flavor = "multi_thread", worker_threads = 3)]
async fn print_drains_the_final_summary_before_closing_its_observers() {
    for mode in [PrintOutput::Json, PrintOutput::Text] {
        let (session, faux, control) = fixture(Some(4), true).await;
        let cancelled = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let entered = Arc::new(Notify::new());
        faux.set_responses(vec![held_summary(
            Arc::clone(&entered),
            Arc::clone(&cancelled),
            Arc::clone(&release),
        )]);
        let (log, _subscription) = events(&session);
        session
            .core
            .background_compaction_boundary(true)
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(20), entered.notified())
            .await
            .unwrap();
        let output = Arc::new(Mutex::new(Vec::<u8>::new()));
        let stdout: SharedWriter = output.clone();
        let stderr: SharedWriter = Arc::new(Mutex::new(Vec::<u8>::new()));
        let owner = Arc::clone(&session);
        let run_control = control.clone();
        let printing = tokio::spawn(async move {
            run_print_mode(
                &owner,
                PrintModeOptions {
                    control: run_control,
                    mode,
                    initial_message: None,
                    initial_images: Vec::new(),
                    messages: Vec::new(),
                },
                stdout,
                stderr,
            )
            .await
        });
        tokio::time::timeout(Duration::from_secs(20), cancelled.notified())
            .await
            .expect("print cancels its pending summary");
        assert!(!printing.is_finished(), "provider cleanup is still owned");
        assert_eq!(control.stopped(), None, "discard is local to the summary");
        release.notify_one();
        let code = tokio::time::timeout(Duration::from_secs(20), printing)
            .await
            .expect("print cleanup finished")
            .unwrap();
        assert_eq!(code, 0);
        assert_eq!(faux.call_count(), 1);
        assert_eq!(control.model_calls(), 1);
        assert_eq!(
            log.lock()
                .unwrap()
                .iter()
                .map(|entry| entry.0)
                .collect::<Vec<_>>(),
            [
                BackgroundCompactionStatus::Started,
                BackgroundCompactionStatus::Discarded
            ]
        );
        let bytes = output.lock().unwrap().clone();
        if mode == PrintOutput::Json {
            let events: Vec<serde_json::Value> = std::str::from_utf8(&bytes)
                .unwrap()
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect();
            let terminal = events
                .iter()
                .find(|event| {
                    event["type"] == "background_compaction" && event["status"] == "discarded"
                })
                .expect("terminal attempt remains in JSON output");
            assert_eq!(terminal["attempts"].as_array().unwrap().len(), 1);
            assert_eq!(terminal["attempts"][0]["dispatched"], true);
            assert!(
                terminal["attempts"][0]["usage"]["totalTokens"]
                    .as_u64()
                    .unwrap()
                    > 0
            );
        } else {
            assert_eq!(std::str::from_utf8(&bytes).unwrap(), "current response\n");
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 3)]
async fn print_awaits_and_reports_a_failed_final_summary_debit() {
    let (session, faux, _) = fixture(None, true).await;
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let observer: bake_agent::AdmissionObserver = {
        let entered = Arc::clone(&entered);
        let release = Arc::clone(&release);
        Arc::new(move |_| {
            let entered = Arc::clone(&entered);
            let release = Arc::clone(&release);
            Box::pin(async move {
                entered.notify_one();
                release.notified().await;
                Err("summary debit sync failed".into())
            })
        })
    };
    let control = RunControl::with_admission_observer(RunLimits::default(), Some(observer));
    *session.core.run_control.lock().unwrap() = Some(control.clone());
    session
        .core
        .background_compaction_boundary(true)
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(20), entered.notified())
        .await
        .expect("summary reserved its debit");
    let signal = session
        .core
        .background_compaction
        .pending
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .signal
        .clone();
    let output = Arc::new(Mutex::new(Vec::<u8>::new()));
    let errors = Arc::new(Mutex::new(Vec::<u8>::new()));
    let stdout: SharedWriter = output.clone();
    let stderr: SharedWriter = errors.clone();
    let owner = Arc::clone(&session);
    let run_control = control.clone();
    let printing = tokio::spawn(async move {
        run_print_mode(
            &owner,
            PrintModeOptions {
                control: run_control,
                mode: PrintOutput::Json,
                initial_message: None,
                initial_images: Vec::new(),
                messages: Vec::new(),
            },
            stdout,
            stderr,
        )
        .await
    });
    tokio::time::timeout(Duration::from_secs(20), signal.cancelled())
        .await
        .expect("print cancelled only its candidate");
    assert!(!printing.is_finished(), "the debit write is still owned");
    assert_eq!(control.stopped(), None);
    assert_eq!(control.model_calls(), 1);
    assert_eq!(faux.call_count(), 0);
    release.notify_one();
    let code = tokio::time::timeout(Duration::from_secs(20), printing)
        .await
        .expect("required debit write completed")
        .unwrap();
    assert_eq!(code, 1);
    assert!(
        matches!(control.stopped(), Some(RunOutcome::Failed { message }) if message.contains("summary debit sync failed"))
    );
    assert_eq!(control.model_calls(), 1, "the debit is not refunded");
    assert_eq!(faux.call_count(), 0, "failed debit prevents dispatch");
    assert!(
        String::from_utf8(errors.lock().unwrap().clone())
            .unwrap()
            .contains("summary debit sync failed")
    );
    let bytes = output.lock().unwrap().clone();
    let terminal: serde_json::Value = std::str::from_utf8(&bytes)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .find(|event| event["type"] == "background_compaction" && event["status"] == "discarded")
        .expect("failed admission still publishes its terminal accounting");
    assert!(
        terminal["attempts"].as_array().unwrap().is_empty(),
        "the failed debit admitted no provider attempt"
    );
}
