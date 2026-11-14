//! `/telecom` and `/spawn`: the person's side of the agent team.
//!
//! `/telecom` shows the board of the repository's Bake agents;
//! `/telecom msg <agent> <text>` messages one as its person (it starts a
//! turn if the agent is idle), and `/telecom broadcast <text>` tells all
//! of them. `/spawn <task>` starts a headless agent on a task, shown in
//! the crew panel above the composer; `/spawn msg <agent> <text>` steers
//! it, `/spawn stop <agent>` ends it, `/spawn interrupt <agent>` stops its
//! run, and `/spawn clear` forgets the agents that exited. None opens a
//! dialog, so none closes one, such as a question the person is
//! answering.

use bake_coding_agent::spawned::Origin;

use super::{Context, Done, Outcome};

const TELECOM_USAGE: &str = "Usage: /telecom, /telecom msg <agent> <text>, or /telecom broadcast <text>; your draft is kept.";
const SPAWN_USAGE: &str = "Usage: /spawn <task>, or /spawn msg|stop|interrupt <agent>, or /spawn clear; your draft is kept.";

/// `argument`'s first word and the rest, trimmed.
fn split(argument: &str) -> (&str, &str) {
    let argument = argument.trim();
    match argument.split_once(char::is_whitespace) {
        Some((word, rest)) => (word, rest.trim()),
        None => (argument, ""),
    }
}

pub(super) async fn telecom(context: &Context, argument: &str) -> Outcome {
    let station = match context.session.telecom() {
        None => {
            return Ok(Done::Note(
                "Telecom is off. Turn on Telecom in /settings, then /new, to join the other Bake sessions in this repository and /spawn agents; BAKE_TELECOM=on turns it on for one run.".to_owned(),
            ));
        }
        Some(Err(reason)) => return Ok(Done::Note(format!("Telecom did not start: {reason}"))),
        Some(Ok(station)) => station.clone(),
    };
    let (action, rest) = split(argument);
    match action {
        "" | "board" => Ok(Done::Note(station.board())),
        "msg" => {
            let (to, text) = split(rest);
            if to.is_empty() || text.is_empty() {
                return Err(TELECOM_USAGE.to_owned());
            }
            let report = station.send(to, text, true).await?;
            Ok(Done::Note(format!("Telecom: {report}.")))
        }
        "broadcast" => {
            if rest.is_empty() {
                return Err(TELECOM_USAGE.to_owned());
            }
            let report = station.broadcast(rest).await?;
            Ok(Done::Note(format!("Telecom: {report}.")))
        }
        _ => Err(TELECOM_USAGE.to_owned()),
    }
}

pub(super) async fn spawn(context: &Context, argument: &str) -> Outcome {
    let Some(host) = context.session.spawned() else {
        return Err(
            "This session cannot spawn agents: turn on Telecom in /settings, then /new (bake docs team); a spawned agent spawns none. Your draft is kept."
                .to_owned(),
        );
    };
    let (action, rest) = split(argument);
    match action {
        "" => Err(SPAWN_USAGE.to_owned()),
        "clear" if rest.is_empty() => {
            host.clear_exited();
            Ok(Done::Quiet)
        }
        // A task may start with one of these words: it is a command only
        // when the next word names an agent.
        "msg" | "stop" | "interrupt" if host.find(split(rest).0).is_ok() => {
            let (who, text) = split(rest);
            let agent = host.find(who)?;
            match action {
                "msg" if text.is_empty() => Err(SPAWN_USAGE.to_owned()),
                "msg" => {
                    host.message(agent.id, text)?;
                    Ok(Done::Note(format!("Sent to {}.", agent.name)))
                }
                "stop" => {
                    host.stop(agent.id)?;
                    Ok(Done::Note(format!("Stopping {}.", agent.name)))
                }
                _ => {
                    host.interrupt(agent.id)?;
                    Ok(Done::Note(format!("Interrupted {}'s run.", agent.name)))
                }
            }
        }
        _ => {
            let agent = host.spawn(argument, None, Origin::Person)?;
            Ok(Done::Note(format!(
                "Spawned {} on \"{}\". It joins telecom as {}; you and your agent are told when it finishes a run.",
                agent.name, agent.task, agent.name
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::split;

    #[test]
    fn arguments_split_on_their_first_word() {
        assert_eq!(
            split("  msg alpha-1  run the tests "),
            ("msg", "alpha-1  run the tests")
        );
        assert_eq!(split("board"), ("board", ""));
        assert_eq!(split(""), ("", ""));
    }
}
