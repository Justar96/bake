# Agent teams

Bake sessions working in the same repository on one machine can form a team through telecom: each takes a callsign, shows what it works on, and can message, ask, and answer the others. A session in the team can also spawn headless Bake agents, shown in the crew panel above the composer, and the model can put questions to you with `ask_user`. Telecom and spawned agents are off by default while their effect on models is measured; `ask_user` is on wherever someone can answer.

## Telecom

Turn telecom on in `/settings`, under Agent team, which saves `"telecom": true` in `settings.json` (`bake docs settings`); it applies from the next session, so follow it with `/new`, `/resume`, or a restart. A top-level session then joins its repository's team when it starts. `BAKE_TELECOM` set to `1`, `on`, `true`, or `yes` turns telecom on for a run, and `0`, `off`, `false`, or `no` turns it off, whatever the setting says. Subagents do not join; they talk to their parent through their own tools. The team is every Bake session whose working directory is in the same Git repository, its worktrees included; a directory outside Git is a team of its own.

A session takes the first free callsign of `alpha`, `bravo`, `charlie`, and so on, or `BAKE_TELECOM_CALLSIGN` when that one is free. A spawned agent takes its parent's callsign and a number, such as `alpha-1`. The callsign is held by a file lock for the life of the process, so a session that exits or crashes leaves the team at once; nothing has to time out.

The model has the `telecom` tool, with these actions:

| Action | What it does |
|---|---|
| `board` | Lists the team: each member's callsign, state (working, idle, or the tool it runs), branch, model, and task |
| `send` | Messages one member, or several separated by commas, and reports who received it |
| `ask` | Asks one member and waits for the reply, 300 seconds by default and at most 3600; with `wait: false` it returns at once and the reply arrives later as a message |
| `reply` | Answers an ask; `askId` may be left out when only one is waiting |
| `broadcast` | Tells every member |
| `status` | Sets this session's task, the line the board shows |

A message is delivered to the recipient's session, which reports it delivered only once the session holds it, so the sender knows who got it. The recipient's terminal shows it as one line, such as `← bravo: the tests pass`, or `← bravo's person: ...` when the person typed it. An idle recipient starts a turn for it, at most six a minute; an ask always starts one. Before each prompt, the model is given a brief of the team when it changed: the whole roster the first time, then who joined or left, held broadcasts, and asks it still owes.

Telecom uses Unix domain sockets in `telecom/` in the Bake home, or in `BAKE_TELECOM_DIR`, readable only by you. Messages travel only between your own processes on this machine. It does not start on Windows.

In the interactive terminal:

- `/telecom` shows the board.
- `/telecom msg <agent> <text>` messages one member as you, not as your model.
- `/telecom broadcast <text>` tells every member.

## Spawned agents

A session in the team can start other Bake agents, each a `bake --mode rpc` process of the same executable working in the same directory (`bake docs rpc`). The model starts one with `spawn_agent`, giving it a self-contained task and, optionally, a `provider/model`; the agent does not see the conversation. It runs with the parent's sandbox and approval mode, joins telecom as `<parent>-<n>`, and spawns no agents of its own. At most six run at once.

Each time an agent finishes a run, the parent session is told, with the agent's task, whether you or the model started it, and its last answer, and starts a turn if it is idle. The terminal shows it as `← alpha-1 finished: ...` or `← alpha-1 failed: ...`. The model messages and asks its agents with `telecom`, and ends one with `stop_agent`, or stops only its current run with `interrupt: true`. A question or approval an agent needs comes to you, marked with the agent's name.

The crew panel above the composer shows each agent while there are any: a spinner and its current tool or line while it works, `?` while it waits for an answer, `+` when idle, `x` when its run failed or its process ended unsuccessfully, with the reason, and `-` once it was stopped or exited cleanly. With `NO_COLOR` set, the panel draws without colour or motion. In the interactive terminal:

- Ctrl+X opens an agent's transcript under the composer, read from its session file without disturbing it and updated as it works; ←→ move between agents, ↑↓ and PgUp/PgDn scroll, and Esc or Ctrl+X closes it.
- `/spawn <task>` starts an agent on a task.
- `/spawn msg <agent> <text>` steers it.
- `/spawn interrupt <agent>` stops its current run.
- `/spawn stop <agent>` ends it.
- `/spawn clear` removes the agents that exited from the panel.

Agents are owned by the session that spawned them: ending or replacing that session stops them, waiting ten seconds for each to shut down before ending it.

## Questions

When someone can answer, in the interactive terminal or over RPC, a top-level session has the `ask_user` tool by default; remove it like any other tool, such as with `-t -ask_user` or `"defaultTools": ["-ask_user"]`. It puts one to four questions to you, each with a short header and two to six options, or none for a typed answer, and `multiSelect` to allow several choices. The terminal shows each question as a list, with "Type my own answer" for an answer of your own; Esc declines the questions, and the model is told so. Over RPC, the questions are put to the client as `select` and `input` requests (`bake docs rpc`). `bake -p` has no one to ask, so the tool is not available there. A spawned agent's questions and approvals reach you through its parent session whether or not the parent has the tool.
