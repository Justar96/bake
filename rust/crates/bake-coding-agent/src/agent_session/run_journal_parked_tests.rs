//! An owned execution's result survives navigation before its waiter resumes.

use super::*;

#[tokio::test]
async fn parked_recoverable_waiters_keep_their_answer_after_branch_navigation() {
    for resume in [false, true] {
        let dir = TempDir::new("run-parked-answer");
        let mut manager = file_session(&dir);
        manager
            .append_message(UserMessage {
                content: UserContent::Text("earlier task".into()),
                timestamp: 1,
            })
            .unwrap();
        let earlier = manager
            .append_message(faux_assistant_message("earlier answer"))
            .unwrap();
        if resume {
            checkpoint_input(&mut manager, "selected", 3, false);
        }
        let h = harness(200_000);
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let (reached, held) = (Arc::clone(&entered), Arc::clone(&release));
        h.faux
            .set_responses(vec![FauxResponseStep::Factory(Arc::new(
                move |_, _, _, _| {
                    let (reached, held) = (Arc::clone(&reached), Arc::clone(&held));
                    Box::pin(async move {
                        reached.notify_one();
                        held.notified().await;
                        Ok(faux_assistant_message("selected answer"))
                    })
                },
            ))]);
        let session = AgentSession::create(options(&h, manager, Settings::default()))
            .await
            .unwrap();
        let mut caller = Box::pin(async {
            if resume {
                session.resume_run("selected").await
            } else {
                session
                    .prompt_recoverable("selected task", Vec::new(), RunLimits::default())
                    .await
            }
        });
        tokio::time::timeout(Duration::from_secs(10), async {
            tokio::select! {
                result = &mut caller => panic!("request ended before its provider barrier: {result:?}"),
                () = entered.notified() => {}
            }
        })
        .await
        .unwrap();
        release.notify_one();
        tokio::time::timeout(Duration::from_secs(10), session.core.wait_for_idle())
            .await
            .expect("owned execution settled while its caller stayed parked");
        session.navigate_tree(&earlier, None).await.unwrap();
        let result = caller.await.unwrap();
        session.shutdown().await;
        assert_eq!(result.outcome, RunOutcome::FinishedTurn, "resume={resume}");
        assert_eq!(
            result.final_message.map(|message| message.content),
            Some(faux_assistant_message("selected answer").content),
            "resume={resume}"
        );
        assert_eq!(h.faux.call_count(), 1, "resume={resume}");
    }
}
