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
