use std::path::{Path, PathBuf};
use std::time::Duration;

use super::*;
use crate::jobs::{JobLimits, JobRegistry};
use crate::owned_work::WorkGate;
use bake_ai::{UserContent, UserContentBlock};

struct Member {
    station: Station,
    jobs: JobRegistry,
    work: WorkGate,
}

fn temp() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("bt-{}", crate::session::id::uuid_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn join(root: &Path, cwd: &Path, session: &str, parent: Option<&str>) -> Member {
    let work = WorkGate::default();
    let jobs = JobRegistry::new(work.clone(), JobLimits::default());
    let station = Station::start(
        StationOptions {
            root: root.to_path_buf(),
            cwd: cwd.to_path_buf(),
            session_id: session.into(),
            callsign: None,
            parent: parent.map(str::to_owned),
            model: Some("faux/model".into()),
        },
        jobs.clone(),
        &work,
    )
    .unwrap();
    Member {
        station,
        jobs,
        work,
    }
}

fn mail_texts(jobs: &JobRegistry) -> Vec<String> {
    jobs.claim_mail()
        .into_iter()
        .map(|message| match message.content {
            UserContent::Blocks(blocks) => blocks
                .into_iter()
                .filter_map(|block| match block {
                    UserContentBlock::Text(text) => Some(text.text),
                    _ => None,
                })
                .collect::<String>(),
            UserContent::Text(text) => text,
        })
        .collect()
}

#[tokio::test]
async fn members_take_callsigns_and_message_each_other() {
    let (root, cwd) = (temp(), temp());
    let alpha = join(&root, &cwd, "s-alpha", None);
    let bravo = join(&root, &cwd, "s-bravo", None);
    let child = join(&root, &cwd, "s-child", Some("alpha"));
    assert_eq!(alpha.station.callsign(), "alpha");
    assert_eq!(bravo.station.callsign(), "bravo");
    assert_eq!(child.station.callsign(), "alpha-1");

    let report = alpha
        .station
        .send("bravo, alpha-1", "rebase first", false)
        .await
        .unwrap();
    assert_eq!(report, "delivered to bravo, alpha-1");
    assert_eq!(
        mail_texts(&bravo.jobs),
        ["[telecom] Message from alpha (also to alpha-1):\nrebase first"]
    );
    assert!(
        alpha
            .station
            .send("zulu", "hi", false)
            .await
            .unwrap_err()
            .contains("no agent \"zulu\"")
    );

    let board = bravo.station.board();
    assert!(board.starts_with("You are bravo [idle"), "{board}");
    assert!(board.contains("- alpha-1 [idle"), "{board}");
    assert!(board.contains("(spawned by alpha)"), "{board}");

    for member in [alpha, bravo, child] {
        member.work.close().await;
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn an_ask_waits_for_its_reply() {
    let (root, cwd) = (temp(), temp());
    let alpha = join(&root, &cwd, "s-alpha", None);
    let bravo = join(&root, &cwd, "s-bravo", None);

    let asking = {
        let station = alpha.station.clone();
        tokio::spawn(async move {
            station
                .ask(
                    "bravo",
                    "Is users.email unique?",
                    Duration::from_secs(10),
                    true,
                    None,
                )
                .await
        })
    };
    // Bravo's model sees the ask, then answers it.
    let mut owed = Vec::new();
    for _ in 0..200 {
        owed = bravo.station.owed();
        if !owed.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let ask_id = owed[0].ask_id.clone();
    let mail = mail_texts(&bravo.jobs);
    assert!(
        mail[0].starts_with(&format!("[telecom] alpha asks you (ask {ask_id})")),
        "{mail:?}"
    );
    assert_eq!(
        bravo
            .station
            .reply("yes, there is an index", None, None)
            .await
            .unwrap(),
        format!("answered alpha ({ask_id})")
    );
    let outcome = asking.await.unwrap().unwrap();
    assert_eq!(
        outcome,
        AskOutcome::Answered {
            ask_id,
            from: "bravo".into(),
            text: "yes, there is an index".into()
        }
    );
    assert!(bravo.station.owed().is_empty());
    assert!(bravo.station.reply("again", None, None).await.is_err());

    for member in [alpha, bravo] {
        member.work.close().await;
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn an_unanswered_ask_arrives_late_as_a_message() {
    let (root, cwd) = (temp(), temp());
    let alpha = join(&root, &cwd, "s-alpha", None);
    let bravo = join(&root, &cwd, "s-bravo", None);
    let outcome = alpha
        .station
        .ask(
            "bravo",
            "ping me when the build is green",
            Duration::from_secs(1),
            false,
            None,
        )
        .await
        .unwrap();
    let AskOutcome::Sent { ask_id, .. } = outcome else {
        panic!("{outcome:?}")
    };
    bravo
        .station
        .reply("green", Some(&ask_id), None)
        .await
        .unwrap();
    assert_eq!(
        mail_texts(&alpha.jobs),
        [format!(
            "[telecom] bravo answered your ask {ask_id}:\ngreen"
        )]
    );
    for member in [alpha, bravo] {
        member.work.close().await;
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn an_ask_fails_when_its_peer_leaves() {
    let (root, cwd) = (temp(), temp());
    let alpha = join(&root, &cwd, "s-alpha", None);
    let bravo = join(&root, &cwd, "s-bravo", None);
    let asking = {
        let station = alpha.station.clone();
        tokio::spawn(async move {
            station
                .ask("bravo", "still there?", Duration::from_secs(30), true, None)
                .await
        })
    };
    while bravo.station.owed().is_empty() {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    bravo.work.close().await;
    let outcome = asking.await.unwrap().unwrap();
    assert!(
        matches!(outcome, AskOutcome::Gone { ref to, .. } if to == "bravo"),
        "{outcome:?}"
    );
    assert!(alpha.station.peers().is_empty());
    alpha.work.close().await;
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn the_brief_lists_the_team_once_then_only_changes() {
    let (root, cwd) = (temp(), temp());
    let alpha = join(&root, &cwd, "s-alpha", None);
    assert!(alpha.station.brief("hi").is_none(), "alone: no brief");
    let bravo = join(&root, &cwd, "s-bravo", None);
    bravo.station.set_status("the api layer");
    // The publisher writes the record on its own task.
    for _ in 0..200 {
        if alpha
            .station
            .peers()
            .first()
            .and_then(|peer| peer.task.clone())
            .is_some()
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let first = alpha.station.brief("refactor the auth module").unwrap();
    let text = mail_text(&first);
    assert!(
        text.starts_with("[telecom] You are alpha. 1 other agent works"),
        "{text}"
    );
    assert!(text.contains("- bravo [idle"), "{text}");
    assert!(text.contains("task: the api layer"), "{text}");
    assert!(!first.display);
    assert_eq!(
        alpha.station.presence().task.as_deref(),
        Some("refactor the auth module")
    );
    assert!(
        alpha.station.brief("next step please").is_none(),
        "nothing changed"
    );

    bravo.station.broadcast("main is frozen").await.unwrap();
    bravo.work.close().await;
    let update = mail_text(&alpha.station.brief("go on with it").unwrap());
    assert_eq!(
        update,
        "[telecom] Team update (you are alpha):\n- bravo left\n[telecom] Broadcasts since your last prompt:\n- bravo: main is frozen"
    );
    alpha.work.close().await;
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn a_reply_from_the_wrong_peer_settles_nothing() {
    let (root, cwd) = (temp(), temp());
    let alpha = join(&root, &cwd, "s-alpha", None);
    let bravo = join(&root, &cwd, "s-bravo", None);
    let charlie = join(&root, &cwd, "s-charlie", None);
    let outcome = alpha
        .station
        .ask("bravo", "question", Duration::from_secs(1), false, None)
        .await
        .unwrap();
    let AskOutcome::Sent { ask_id, .. } = outcome else {
        panic!()
    };
    let forged = Frame::new(
        Sender {
            session: "s-charlie".into(),
            callsign: "charlie".into(),
        },
        Body::Reply {
            ask_id: ask_id.clone(),
            text: "forged".into(),
        },
    );
    let socket = alpha.station.team().socket("alpha");
    assert!(matches!(
        endpoint::deliver(&socket, &forged).await,
        Ok(Outcome::Refused { .. })
    ));
    assert!(mail_texts(&alpha.jobs).is_empty());
    for member in [alpha, bravo, charlie] {
        member.work.close().await;
    }
    std::fs::remove_dir_all(root).unwrap();
}

/// Waits for `ready`, polling an observable condition.
async fn until(mut ready: impl FnMut() -> bool) {
    for _ in 0..2000 {
        if ready() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("the condition never held");
}

fn ask_in(
    member: &Member,
    to: &str,
    timeout: Duration,
    signal: Option<bake_ai::AbortSignal>,
) -> tokio::task::JoinHandle<Result<AskOutcome, String>> {
    let station = member.station.clone();
    let to = to.to_owned();
    tokio::spawn(async move {
        station
            .ask(&to, "can you check this?", timeout, true, signal)
            .await
    })
}

/// A member whose lock and record are held but who never listens: every
/// delivery to it fails to connect.
fn ghost(team: &directory::Team, callsign: &str) -> directory::Membership {
    let membership = team.claim([callsign]).unwrap().unwrap();
    membership
        .publish(&Presence {
            v: WIRE_VERSION,
            session: format!("s-{callsign}"),
            callsign: callsign.into(),
            pid: std::process::id(),
            checkout: "/repo".into(),
            branch: None,
            task: None,
            activity: Activity::Idle,
            since: 0,
            tool: None,
            parent: None,
            model: None,
            started_at: 0,
        })
        .unwrap();
    membership
}

#[tokio::test]
async fn crossed_asks_release_each_other_and_hold_each_others_ask_in_mail() {
    let (root, cwd) = (temp(), temp());
    let alpha = join(&root, &cwd, "s-alpha", None);
    let bravo = join(&root, &cwd, "s-bravo", None);
    let long = Duration::from_secs(600);
    let (from_alpha, from_bravo) = (
        ask_in(&alpha, "bravo", long, None),
        ask_in(&bravo, "alpha", long, None),
    );
    let alpha_outcome = from_alpha.await.unwrap().unwrap();
    let bravo_outcome = from_bravo.await.unwrap().unwrap();
    let AskOutcome::Interrupted {
        ask_id: alpha_ask,
        to,
    } = alpha_outcome
    else {
        panic!("{alpha_outcome:?}")
    };
    assert_eq!(to, "bravo");
    let AskOutcome::Interrupted {
        ask_id: bravo_ask, ..
    } = bravo_outcome
    else {
        panic!("{bravo_outcome:?}")
    };
    let alpha_mail = mail_texts(&alpha.jobs);
    let bravo_mail = mail_texts(&bravo.jobs);
    assert!(
        alpha_mail[0].starts_with(&format!("[telecom] bravo asks you (ask {bravo_ask})")),
        "{alpha_mail:?}"
    );
    assert!(
        bravo_mail[0].starts_with(&format!("[telecom] alpha asks you (ask {alpha_ask})")),
        "{bravo_mail:?}"
    );
    // Both asks stay open: each answer arrives as a message.
    bravo
        .station
        .reply("checked", Some(&alpha_ask), None)
        .await
        .unwrap();
    assert_eq!(
        mail_texts(&alpha.jobs),
        [format!(
            "[telecom] bravo answered your ask {alpha_ask}:\nchecked"
        )]
    );
    for member in [alpha, bravo] {
        member.work.close().await;
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn a_ring_of_three_waiting_asks_releases_every_member() {
    let (root, cwd) = (temp(), temp());
    let alpha = join(&root, &cwd, "s-alpha", None);
    let bravo = join(&root, &cwd, "s-bravo", None);
    let charlie = join(&root, &cwd, "s-charlie", None);
    let long = Duration::from_secs(600);
    let asks = [
        ask_in(&alpha, "bravo", long, None),
        ask_in(&bravo, "charlie", long, None),
        ask_in(&charlie, "alpha", long, None),
    ];
    for ask in asks {
        let outcome = ask.await.unwrap().unwrap();
        assert!(
            matches!(outcome, AskOutcome::Interrupted { .. }),
            "{outcome:?}"
        );
    }
    for member in [&alpha, &bravo, &charlie] {
        assert_eq!(member.station.owed().len(), 1);
    }
    for member in [alpha, bravo, charlie] {
        member.work.close().await;
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn an_ask_made_with_mail_waiting_returns_at_once_after_delivering_it() {
    let (root, cwd) = (temp(), temp());
    let alpha = join(&root, &cwd, "s-alpha", None);
    let bravo = join(&root, &cwd, "s-bravo", None);
    bravo
        .station
        .send("alpha", "the schema changed", false)
        .await
        .unwrap();
    let outcome = alpha
        .station
        .ask(
            "bravo",
            "which table?",
            Duration::from_secs(600),
            true,
            None,
        )
        .await
        .unwrap();
    assert!(
        matches!(outcome, AskOutcome::Interrupted { .. }),
        "{outcome:?}"
    );
    assert_eq!(bravo.station.owed().len(), 1, "the ask was delivered");
    assert_eq!(
        mail_texts(&alpha.jobs),
        ["[telecom] Message from bravo:\nthe schema changed"]
    );
    for member in [alpha, bravo] {
        member.work.close().await;
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn a_notice_posted_during_the_wait_ends_it_and_the_answer_comes_later() {
    let (root, cwd) = (temp(), temp());
    let alpha = join(&root, &cwd, "s-alpha", None);
    let bravo = join(&root, &cwd, "s-bravo", None);
    let asking = ask_in(&alpha, "bravo", Duration::from_secs(600), None);
    until(|| !bravo.station.owed().is_empty()).await;
    alpha
        .jobs
        .post(message("background job 3 finished".into(), true, json!({})))
        .unwrap();
    let outcome = asking.await.unwrap().unwrap();
    let AskOutcome::Interrupted { ask_id, .. } = outcome else {
        panic!("{outcome:?}")
    };
    assert_eq!(mail_texts(&alpha.jobs), ["background job 3 finished"]);
    bravo.station.reply("done", None, None).await.unwrap();
    assert_eq!(
        mail_texts(&alpha.jobs),
        [format!("[telecom] bravo answered your ask {ask_id}:\ndone")]
    );
    for member in [alpha, bravo] {
        member.work.close().await;
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn an_answer_sent_as_the_wait_ends_is_taken_not_dropped() {
    let (root, cwd) = (temp(), temp());
    let alpha = join(&root, &cwd, "s-alpha", None);
    let bravo = join(&root, &cwd, "s-bravo", None);
    let peer = alpha.station.resolve("bravo").unwrap();
    for _ in 0..200 {
        // The answer is already sent when mail ends the wait.
        let (answer, mut answered) = oneshot::channel();
        answer.send("yes".to_owned()).unwrap();
        alpha
            .jobs
            .post(message("a notice".into(), true, json!({})))
            .unwrap();
        let outcome = alpha
            .station
            .await_answer(&peer, &mut answered, Duration::ZERO, None)
            .await;
        assert!(
            matches!(outcome, AskOutcome::Answered { ref text, .. } if text == "yes"),
            "{outcome:?}"
        );
        alpha.jobs.claim_mail();
    }
    for member in [alpha, bravo] {
        member.work.close().await;
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn a_reply_racing_the_deadline_is_answered_or_arrives_as_a_message() {
    let (root, cwd) = (temp(), temp());
    let alpha = join(&root, &cwd, "s-alpha", None);
    let bravo = join(&root, &cwd, "s-bravo", None);
    for round in 0..60u64 {
        let asking = ask_in(&alpha, "bravo", Duration::from_millis(round % 4), None);
        until(|| !bravo.station.owed().is_empty()).await;
        let reply = format!("answer {round}");
        bravo.station.reply(&reply, None, None).await.unwrap();
        let outcome = asking.await.unwrap().unwrap();
        let mail = mail_texts(&alpha.jobs);
        match outcome {
            AskOutcome::Answered { text, .. } => {
                assert_eq!(text, reply);
                assert!(mail.is_empty(), "{mail:?}");
            }
            AskOutcome::TimedOut { ask_id, .. } => assert_eq!(
                mail,
                [format!(
                    "[telecom] bravo answered your ask {ask_id}:\n{reply}"
                )]
            ),
            other => panic!("{other:?}"),
        }
    }
    for member in [alpha, bravo] {
        member.work.close().await;
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn a_cancelled_ask_is_withdrawn_and_a_late_answer_still_arrives() {
    let (root, cwd) = (temp(), temp());
    let alpha = join(&root, &cwd, "s-alpha", None);
    let bravo = join(&root, &cwd, "s-bravo", None);
    let controller = bake_ai::AbortController::new();
    let asking = ask_in(
        &alpha,
        "bravo",
        Duration::from_secs(600),
        Some(controller.signal()),
    );
    until(|| !bravo.station.owed().is_empty()).await;
    let ask_id = bravo.station.owed()[0].ask_id.clone();
    controller.abort();
    let outcome = asking.await.unwrap().unwrap();
    assert_eq!(
        outcome,
        AskOutcome::Cancelled {
            ask_id: ask_id.clone()
        }
    );
    until(|| bravo.station.owed().is_empty()).await;
    assert_eq!(
        bravo
            .station
            .reply("too late?", Some(&ask_id), None)
            .await
            .unwrap(),
        format!("alpha withdrew ask {ask_id}; nothing more to do")
    );
    // An answer that crossed the withdrawal is not thrown away.
    let crossed = Frame::new(
        Sender {
            session: "s-bravo".into(),
            callsign: "bravo".into(),
        },
        Body::Reply {
            ask_id: ask_id.clone(),
            text: "it is fine".into(),
        },
    );
    let socket = alpha.station.team().socket("alpha");
    assert_eq!(
        endpoint::deliver(&socket, &crossed).await,
        Ok(Outcome::Accepted)
    );
    assert_eq!(
        mail_texts(&alpha.jobs),
        [format!(
            "[telecom] bravo answered your earlier ask {ask_id}:\nit is fine"
        )]
    );
    for member in [alpha, bravo] {
        member.work.close().await;
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn this_sessions_own_shutdown_ends_its_wait_as_cancelled() {
    let (root, cwd) = (temp(), temp());
    let alpha = join(&root, &cwd, "s-alpha", None);
    let bravo = join(&root, &cwd, "s-bravo", None);
    let asking = ask_in(&alpha, "bravo", Duration::from_secs(600), None);
    until(|| !bravo.station.owed().is_empty()).await;
    alpha.work.close().await;
    let outcome = asking.await.unwrap().unwrap();
    assert!(
        matches!(outcome, AskOutcome::Cancelled { .. }),
        "{outcome:?}"
    );
    bravo.work.close().await;
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn a_peer_is_tracked_by_session_not_by_callsign() {
    let (root, cwd) = (temp(), temp());
    let alpha = join(&root, &cwd, "s-alpha", None);
    let bravo = join(&root, &cwd, "s-bravo", None);
    let asking = ask_in(&alpha, "bravo", Duration::from_secs(600), None);
    until(|| !bravo.station.owed().is_empty()).await;
    // Someone else's reply under bravo's callsign settles nothing.
    let ask_id = bravo.station.owed()[0].ask_id.clone();
    let forged = Frame::new(
        Sender {
            session: "s-other".into(),
            callsign: "bravo".into(),
        },
        Body::Reply {
            ask_id,
            text: "forged".into(),
        },
    );
    let socket = alpha.station.team().socket("alpha");
    assert!(matches!(
        endpoint::deliver(&socket, &forged).await,
        Ok(Outcome::Refused { .. })
    ));
    // Bravo leaves and a new session takes its callsign: the ask's peer
    // is still gone.
    bravo.work.close().await;
    let successor = join(&root, &cwd, "s-successor", None);
    assert_eq!(successor.station.callsign(), "bravo");
    let outcome = asking.await.unwrap().unwrap();
    assert!(
        matches!(outcome, AskOutcome::Gone { ref to, .. } if to == "bravo"),
        "{outcome:?}"
    );
    assert!(successor.station.owed().is_empty());
    for member in [alpha, successor] {
        member.work.close().await;
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn an_ask_whose_asker_left_is_dropped_and_blocks_no_reply() {
    let (root, cwd) = (temp(), temp());
    let alpha = join(&root, &cwd, "s-alpha", None);
    let bravo = join(&root, &cwd, "s-bravo", None);
    let charlie = join(&root, &cwd, "s-charlie", None);
    let AskOutcome::Sent { ask_id: stale, .. } = alpha
        .station
        .ask("bravo", "old question", Duration::from_secs(1), false, None)
        .await
        .unwrap()
    else {
        panic!()
    };
    let AskOutcome::Sent { ask_id: open, .. } = charlie
        .station
        .ask("bravo", "new question", Duration::from_secs(1), false, None)
        .await
        .unwrap()
    else {
        panic!()
    };
    alpha.work.close().await;
    let board = bravo.station.board();
    assert!(!board.contains(&stale), "{board}");
    assert!(board.contains(&open), "{board}");
    assert_eq!(
        bravo.station.reply("answer", None, None).await.unwrap(),
        format!("answered charlie ({open})")
    );
    assert_eq!(
        bravo
            .station
            .reply("answer", Some(&stale), None)
            .await
            .unwrap(),
        format!("alpha is gone; nothing more to do for ask {stale}")
    );
    for member in [bravo, charlie] {
        member.work.close().await;
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn send_names_each_member_once_and_batches_past_the_group_cap() {
    let (root, cwd) = (temp(), temp());
    let alpha = join(&root, &cwd, "s-alpha", None);
    let peers: Vec<Member> = (0..wire::GROUP_CAP + 1)
        .map(|n| join(&root, &cwd, &format!("s-peer{n}"), None))
        .collect();
    let names: Vec<String> = peers.iter().map(|peer| peer.station.callsign()).collect();
    let report = alpha
        .station
        .send(
            &format!("bravo, bravo,{}", names.join(",")),
            "rebase",
            false,
        )
        .await
        .unwrap();
    assert_eq!(report, format!("delivered to {}", names.join(", ")));
    for peer in &peers {
        assert_eq!(
            mail_texts(&peer.jobs).len(),
            1,
            "{}",
            peer.station.callsign()
        );
    }
    alpha.work.close().await;
    for peer in peers {
        peer.work.close().await;
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn a_broadcast_names_the_members_it_did_not_reach() {
    let (root, cwd) = (temp(), temp());
    let alpha = join(&root, &cwd, "s-alpha", None);
    let bravo = join(&root, &cwd, "s-bravo", None);
    let zulu = ghost(alpha.station.team(), "zulu");
    let report = alpha.station.broadcast("main is frozen").await.unwrap();
    assert!(
        report.starts_with("broadcast to bravo (failed: zulu: cannot connect"),
        "{report}"
    );
    bravo.work.close().await;
    let error = alpha.station.broadcast("again").await.unwrap_err();
    assert!(
        error.starts_with("not delivered (zulu: cannot connect"),
        "{error}"
    );
    drop(zulu);
    alpha.work.close().await;
    std::fs::remove_dir_all(root).unwrap();
}

fn mail_text(message: &CustomMessage) -> String {
    match &message.content {
        UserContent::Blocks(blocks) => blocks
            .iter()
            .filter_map(|block| match block {
                UserContentBlock::Text(text) => Some(text.text.clone()),
                _ => None,
            })
            .collect(),
        UserContent::Text(text) => text.clone(),
    }
}
