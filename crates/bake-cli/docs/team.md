# Agent teams

Bake sessions working in the same repository on one machine can form a team through telecom: each takes a callsign, shows what it works on, and can message, ask, and answer the others. A session in the team can also spawn headless Bake agents, shown in the crew panel above the composer, and the model can put questions to you with `ask_user`. Telecom and spawned agents are off by default while their effect on models is measured; `ask_user` is on wherever someone can answer.

## Telecom

Turn telecom on in `/settings`, under Agent team, which saves `"telecom": true` in `settings.json` (`bake docs settings`); it applies from the next session, so follow it with `/new`, `/resume`, or a restart. A top-level session then joins its repository's team when it starts. `BAKE_TELECOM` set to `1`, `on`, `true`, or `yes` turns telecom on for a run, and `0`, `off`, `false`, or `no` turns it off, whatever the setting says. Subagents do not join; they talk to their parent through their own tools. The team is every Bake session whose working directory is in the same Git repository, its worktrees included; a directory outside Git is a team of its own.

A session takes `BAKE_TELECOM_CALLSIGN` when that one is free, else a short callsign drawn at random among the free ones of a pool, such as `nova`, `rook`, or `ember`, and then their numbered forms (`nova2`); each session draws in its own order, so callsigns differ from run to run. `callsigns` in `settings.json` replaces the built-in pool with your own list, such as `"callsigns": ["kite", "vega", "orca"]`: each name is lowercased, must be 1 to 16 letters, digits, `-`, or `_`, starting with a letter or digit, and is used once; names that are not are skipped, and a list with none left keeps the built-in pool. The status line shows the session's callsign before the model, bold in a colour of its own: each name of a pool has a different hue, and a spawned agent's turns its parent's. A spawned agent takes its parent's callsign and a number, such as `alpha-1`. The callsign is held by a file lock for the life of the process, so a session that exits or crashes leaves the team at once; nothing has to time out.

The model has the `telecom` tool, with these actions:

| Action | What it does |
|---|---|
| `board` | Lists the team: each member's callsign, state (working, idle, or the tool it runs), branch, model, and task |
| `send` | Messages one member, or several separated by commas, and reports who received it |
| `ask` | Asks one member and waits for the reply, 300 seconds by default and at most 3600; with `wait: false` or `timeoutSec: 0` it returns at once. A reply that comes after the wait arrives as a message |
| `reply` | Answers an ask; `askId` may be left out when only one is waiting. An ask whose asker left or withdrew it needs no answer |
| `broadcast` | Tells every member and reports any it did not reach |
| `status` | Sets this session's task, the line the board shows |

A message is delivered to the recipient's session, which reports it delivered only once the session holds it, so the sender knows who got it. The recipient's terminal shows it as one line, such as `← bravo: the tests pass`, or `← bravo's person: ...` when the person typed it. An idle recipient starts a turn for it. Messages and asks from other members' models share a limit of six such turns a minute; past it, a message or ask is still delivered but held, the sender is told so, and the recipient reads it at its next step if it is working, or in a turn of its own once the minute's window reopens. Your messages and the replies to the session's own asks always start a turn. A waiting ask ends early when anything else arrives for the asking session, such as another member's ask or a finished job, so two members asking each other never wait on each other; the ask stays open and its reply arrives as a message. The model is told what changed in the team, the whole roster the first time, then who joined or left, new tasks, broadcasts (the latest 20), and asks it still owes, before a prompt, in a turn opened for a message or notice, and between the steps of a run; a broadcast never starts a turn by itself.

In print mode (`bake -p`), an ask the model sent without waiting for, or whose wait ended, counts as running work for as long as the ask would have waited, from when it was sent (300 seconds for one sent without waiting), or until the member asked sends a message or an ask: Bake waits for its reply within the same 10-minute bound as background jobs and runs a turn for it, or tells the model if the member left first. Past that the ask stays open, but Bake no longer waits for it. Before it exits, the session leaves the team, so no message is reported delivered that it will not read (`bake docs headless`).

Telecom uses Unix domain sockets in `telecom/` in the Bake home, or in `BAKE_TELECOM_DIR`, readable only by you. Messages travel only between your own processes on this machine. It does not start on Windows.

In the interactive terminal:

- `/telecom` shows the board.
- `/telecom msg <agent> <text>` messages one member as you, not as your model.
- `/telecom broadcast <text>` tells every member.

## Spawned agents

A session in the team can start other Bake agents, each a `bake --mode rpc` process of the same executable working in the same directory (`bake docs rpc`). The model starts one with `spawn_agent`, giving it a self-contained task and, optionally, a `provider/model`; the agent does not see the conversation. It runs with the parent's sandbox and approval mode, joins telecom as `<parent>-<n>`, and spawns no agents of its own. At most six run at once; an agent being stopped no longer counts. Past that, `spawn_agent` and `/spawn` queue the agent, up to 32, and it starts, oldest first, when a running one exits or is stopped; stopping a queued agent cancels it.

Each time an agent finishes a run, the parent session is told, with the agent's task, whether you or the model started it, and its last answer, and starts a turn if it is idle; while agents are queued, the notice says that stopping a finished agent lets one start. The parent is also told, once, when an agent cannot start, refuses its task, or exits without being stopped, and when an agent has waited 60 seconds for your answer to its question or approval. The terminal shows these as `← alpha-1 finished: ...`, `← alpha-1 was interrupted: ...`, `← alpha-1 failed: ...`, `← alpha-1 exited: ...`, or `← alpha-1 waits for you to ...`. The model messages and asks its agents with `telecom`, and ends one with `stop_agent`, or stops only its current run with `interrupt: true`. A question or approval an agent needs comes to you, marked with the agent's name; it is withdrawn when the agent's run ends or the agent stops or exits. A message you send an agent that it refuses, such as during a compaction, is sent once more as a follow-up; if that is refused too, the panel says it was not delivered.

The crew panel above the composer summarises spawned agents while there are any: a compact header with grouped state counts (`active`, `asking`, `failed`, `queued`, `done`), the model when all listed agents share one, `+N more` when agents are hidden, and `Ctrl+X view` when it fits, followed by up to three agents with the busy ones first. When every agent is idle or exited, only the header row shows; Ctrl+X still opens their transcripts, whose header also shows the agent's model. Marks match the transcript and tasks list (`●` while starting or working, `?` while waiting for an answer, `✗` when its run failed or its process ended unsuccessfully, `○` while queued, `✓` when idle, and `-` once stopped or exited cleanly; `*`, `?`, `x`, `~`, `+`, `-` under the classic frame), alongside each agent's callsign, model (when listed agents differ), current tool and last line or reason, and elapsed time. With `NO_COLOR` set, the panel draws without colour or motion. In the interactive terminal:

- Ctrl+X opens an agent's transcript under the composer, read from its session file without disturbing it and updated as it works; ←→ move between agents, ↑↓ and PgUp/PgDn scroll, and Esc or Ctrl+X closes it.
- `/spawn <task>` starts an agent on a task.
- `/spawn msg <agent> <text>` steers it.
- `/spawn interrupt <agent>` stops its current run.
- `/spawn stop <agent>` ends it.
- `/spawn clear` removes the agents that exited from the panel.

Agents are owned by the session that spawned them: ending or replacing that session stops them. An agent being stopped has ten seconds from the stop to shut down, whether or not it is still writing, and is then ended with its processes.

## Questions

When someone can answer, in the interactive terminal or over RPC, a top-level session has the `ask_user` tool by default; remove it like any other tool, such as with `-t -ask_user` or `"defaultTools": ["-ask_user"]`. It puts one to four questions to you, each with a short header and two to six options, or none for a typed answer, and `multiSelect` to allow several choices. The terminal shows each question with its header, progress (`Question 2 of 3`), and full wrapped text above numbered options (`1.`–`9.`), with descriptions wrapped below when they do not fit on one line, and a separated `Other: type an answer…` row; press `1`–`9` or `Enter` to select, or `1`–`9`/`Space` to toggle checkboxes and `Enter` (or `Done`) to finish in multi-select. Esc declines the questions, and the model is told so. Over RPC, the questions are put to the client as `select` and `input` requests (`bake docs rpc`). `bake -p` has no one to ask, so the tool is not available there. A spawned agent's questions and approvals reach you through its parent session whether or not the parent has the tool.
