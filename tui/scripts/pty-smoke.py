#!/usr/bin/env python3
"""Drive the built `tui` profile through a real POSIX terminal.

Coverage is split into named scenarios. `--list` prints them, `--only NAME`
runs one with its prerequisites, and every wait carries a description, so a
timeout names the condition that never arrived instead of dumping a screen and
leaving the reader to guess which of sixty anonymous predicates failed.

    tui/scripts/pty-smoke.py                 # every scenario, recorded replay
    tui/scripts/pty-smoke.py --list
    tui/scripts/pty-smoke.py --only cancel   # that scenario and its prerequisites
    tui/scripts/pty-smoke.py --trace         # print each step as it is satisfied
    tui/scripts/pty-smoke.py --live node24   # real API on the engine floor

A failing step writes the whole terminal transcript under `tui/.smoke/` and
prints its tail, so the screen that produced the failure survives the run.
"""
import argparse
import json
import os
from pathlib import Path
import re
import select
import shutil
import signal
import struct
import subprocess
import sys
import tempfile
import termios
import time
import fcntl
from contextlib import contextmanager
from dataclasses import dataclass, field
from typing import Callable

ROOT = Path(__file__).resolve().parents[2]
FIXTURE = ROOT / 'snapshots/session/bash-tool-turn/session.v3.jsonl'
ARTIFACTS = ROOT / 'tui/.smoke'
ANSI = re.compile(r'\x1b\][^\x07]*(?:\x07|\x1b\\)|\x1b\[[0-?]*[ -/]*[@-~]')
# The tool call the recorded turn makes; its absence proves input was not submitted.
BASH_ROW = '⚙ bash'


def events(path):
    """Read a session log.

    @param path - the JSONL file to read.
    @returns every committed event, in log order.
    """
    return [json.loads(line) for line in Path(path).read_text().splitlines() if line]


class StepFailed(AssertionError):
    """A named terminal step that never happened, reported with the screen that did."""


@dataclass
class Options:
    """Per-run limits and diagnostics, shared by every terminal a run opens."""

    step: float = 30.0
    budget: float = 120.0
    trace: bool = False
    artifacts: Path = ARTIFACTS


class Terminal:
    """A `dsh` child driven through a PTY, waited on by named condition.

    Each wait polls the child's output with the ANSI escapes stripped, which is
    what the assertions read. Failure raises `StepFailed` naming the step, why
    it stopped, whether the child is still alive, and where the transcript
    landed.
    """

    def __init__(self, label: str, command, cwd, env, options: Options):
        self.label = label
        self.options = options
        self.master, self.slave = os.openpty()
        self.modes = termios.tcgetattr(self.slave)
        fcntl.ioctl(self.slave, termios.TIOCSWINSZ, struct.pack('HHHH', 40, 120, 0, 0))
        self.child = subprocess.Popen(command, cwd=cwd, env=env, stdin=self.slave,
                                      stdout=self.slave, stderr=self.slave, start_new_session=True)
        self.output = bytearray()
        self.deadline = time.monotonic() + options.budget
        self.steps = 0

    @property
    def text(self) -> str:
        """The child's output so far with ANSI escapes removed."""
        return ANSI.sub('', self.output.decode(errors='replace'))

    def mark(self) -> int:
        """Current end of the screen, for waits that must ignore earlier output.

        @returns the offset to slice the screen from.
        """
        return len(self.text)

    def read(self, timeout: float) -> bool:
        """Absorb whatever the child has written.

        @param timeout - seconds to block waiting for output.
        @returns whether anything arrived.
        """
        ready, _, _ = select.select([self.master], [], [], timeout)
        if not ready:
            return False
        self.output.extend(os.read(self.master, 65536))
        return True

    def wait(self, description: str, predicate: Callable[[str], bool], timeout: float | None = None) -> str:
        """Block until the screen satisfies a named condition.

        @param description - what is being waited for, reported verbatim on failure.
        @param predicate - reads the cleaned screen and returns whether the step happened.
        @param timeout - seconds this single step may take; the run default otherwise.
        @returns the screen once the condition holds.
        """
        started = time.monotonic()
        limit = started + (self.options.step if timeout is None else timeout)
        while not predicate(self.text):
            if b'failed to import' in self.output:
                self.fail(description, 'a profile plugin failed to import')
            now = time.monotonic()
            if now >= self.deadline:
                self.fail(description, f'the terminal budget of {self.options.budget:g}s ran out')
            if now >= limit:
                self.fail(description, f'no match within {limit - started:g}s')
            if not self.read(0.05) and self.child.poll() is not None:
                self.fail(description, f'the process exited with code {self.child.returncode}')
        self.steps += 1
        self.trace(f'ok   {description}', time.monotonic() - started)
        return self.text

    def expect(self, *needles: str, after: int = 0, timeout: float | None = None) -> str:
        """Wait for every substring to be on screen.

        @param needles - substrings that must all be present.
        @param after - ignore the screen before this offset, from `mark()`.
        @param timeout - seconds this step may take.
        @returns the screen once they are.
        """
        shown = ' and '.join(repr(needle) for needle in needles)
        where = f' after offset {after}' if after else ''
        return self.wait(f'screen shows {shown}{where}',
                         lambda text: all(needle in text[after:] for needle in needles), timeout)

    def search(self, pattern: str, after: int = 0, timeout: float | None = None) -> re.Match:
        """Wait for a pattern to match the screen.

        @param pattern - the regular expression to match.
        @param after - ignore the screen before this offset, from `mark()`.
        @param timeout - seconds this step may take.
        @returns the match, taken from the screen that satisfied the wait.
        """
        self.wait(f'screen matches /{pattern}/' + (f' after offset {after}' if after else ''),
                  lambda text: re.search(pattern, text[after:]) is not None, timeout)
        return re.search(pattern, self.text[after:])

    def follows(self, later: str, earlier: str, timeout: float | None = None) -> str:
        """Wait until the last `later` on screen comes after the last `earlier`.

        @param later - the marker that must appear last.
        @param earlier - the marker it must come after.
        @param timeout - seconds this step may take.
        @returns the screen once the order holds.
        """
        return self.wait(f'{later!r} returns after the last {earlier!r}',
                         lambda text: earlier in text and text.rfind(later) > text.rfind(earlier), timeout)

    def ready(self) -> str:
        """Wait for a mounted app: the status line drawn and paste mode enabled.

        Keys typed before Ink enables bracketed paste are swallowed by the
        terminal and never reach `useInput`, so every scenario starts here.

        @returns the screen once the app accepts input.
        """
        return self.wait('the app to mount and enable bracketed paste',
                         lambda text: '○ Ready' in text and b'\x1b[?2004h' in self.output)

    def send(self, data: bytes, note: str | None = None) -> None:
        """Type into the terminal.

        @param data - the bytes to write, including escape sequences.
        @param note - what the keys mean, for the trace.
        """
        self.trace(f'send {note or data!r}')
        os.write(self.master, data)

    def check(self, description: str, condition: bool, reason: str = 'it did not hold') -> None:
        """Assert a condition about the terminal, reporting the screen when it fails.

        @param description - what was expected.
        @param condition - the expectation's truth.
        @param reason - what happened instead.
        """
        if not condition:
            self.fail(description, reason)
        self.trace(f'ok   {description}')

    def refuse(self, description: str, condition: bool) -> None:
        """Assert something has *not* happened yet.

        @param description - what must not have happened.
        @param condition - whether it did.
        """
        self.check(f'not: {description}', not condition, 'it happened')

    def trace(self, line: str, elapsed: float | None = None) -> None:
        """Report a step to stderr under `--trace`, keeping stdout to results.

        @param line - the step description.
        @param elapsed - seconds it took, when it was a wait.
        """
        if self.options.trace:
            timing = f' ({elapsed * 1000:.0f}ms)' if elapsed is not None else ''
            print(f'  [{self.label}] {line}{timing}', file=sys.stderr)

    def save(self) -> Path:
        """Write the whole transcript where a reader can open it.

        @returns the transcript path.
        """
        self.options.artifacts.mkdir(parents=True, exist_ok=True)
        path = self.options.artifacts / f'{self.label}.log'
        path.write_text(self.text)
        return path

    def fail(self, description: str, reason: str) -> None:
        """Raise a failure naming the step, the process state, and the screen.

        @param description - the step that did not happen.
        @param reason - why waiting stopped.
        """
        alive = self.child.poll()
        tail = '\n'.join(f'  | {line}' for line in self.text.splitlines()[-30:])
        raise StepFailed(
            f'waiting for {description}\n'
            f'  reason:  {reason}\n'
            f'  process: {"running" if alive is None else f"exited with code {alive}"}\n'
            f'  steps:   {self.steps} satisfied before this one\n'
            f'  screen:  {self.save()} (last lines below)\n{tail}')

    def quit(self) -> str:
        """Exit through the double Ctrl-C the product documents, then verify teardown.

        Terminal modes, bracketed paste, and the exit code are all owned by
        `releaseTerminal()`; checking them here is what proves a change to
        teardown kept every path working.

        @returns the whole transcript.
        """
        self.send(b'\x03', 'Ctrl-C')
        self.expect('Press Ctrl-C again')
        self.send(b'\x03', 'Ctrl-C again')
        limit = time.monotonic() + self.options.step
        while self.child.poll() is None:
            if time.monotonic() >= limit:
                self.fail('the process to exit after the second Ctrl-C', f'still running after {self.options.step:g}s')
            self.read(0.05)
        while self.read(0):
            pass
        self.check('a clean exit', self.child.returncode == 0, f'exit code {self.child.returncode}')
        self.check('terminal modes to be restored', termios.tcgetattr(self.slave) == self.modes,
                   'the child left the tty in a different mode')
        self.check('bracketed paste to be released',
                   b'\x1b[?2004h' in self.output and b'\x1b[?2004l' in self.output,
                   'paste mode was enabled but never released')
        return self.text

    def close(self) -> None:
        """Kill any surviving process group and release the pty."""
        if self.child.poll() is None:
            os.killpg(self.child.pid, signal.SIGKILL)
            self.child.wait()
        os.close(self.master)
        os.close(self.slave)


@dataclass
class Scenario:
    """One selectable terminal scenario and what it needs to have run first."""

    name: str
    summary: str
    requires: tuple[str, ...]
    replay_only: bool
    body: Callable[['Run'], None]


SCENARIOS: dict[str, Scenario] = {}


def scenario(summary: str, requires: tuple[str, ...] = (), replay_only: bool = False):
    """Register a scenario under its function name with dashes for underscores.

    @param summary - what the scenario proves, printed by `--list` and on pass.
    @param requires - scenarios whose state this one reads.
    @param replay_only - whether it depends on the recorded fixture rather than a live model.
    @returns the decorator that registers the function.
    """
    def register(body):
        name = body.__name__.replace('_', '-')
        SCENARIOS[name] = Scenario(name, summary, requires, replay_only, body)
        return body
    return register


class Run:
    """The temporary home, workspace, profile, and overlay every scenario shares."""

    def __init__(self, root: Path, args, options: Options):
        self.root = root
        self.live = args.live
        self.node = args.node
        self.options = options
        self.state: dict = {}
        self.home = root / 'home'
        self.workspace = root / 'workspace'
        self.workspace.mkdir()
        self.sessions_root = self.home / 'sessions'
        profile = self.home / 'profiles/tui'
        profile.mkdir(parents=True)
        (profile / 'package.json').write_text(json.dumps({
            'name': 'tui-replay-profile', 'private': True,
            'dsh': {'profile': {'bundles': ['@deepseek-ai/dsh-base', '@deepseek-ai/dsh-headless']}},
        }))
        self.overlay = root / 'replay.patch.yml'
        self.recorded = events(FIXTURE)
        self.prompt = next(e['data']['content'][0]['text'] for e in self.recorded
                           if e['type'] == 'user/message' and e['data']['source']['kind'] == 'user')
        self.env = {**os.environ, 'DSH_HOME': str(self.home), 'DSH_AGENTS_HOME': str(root / 'agents'),
                    'TERM': 'xterm-256color', 'NO_COLOR': '1'}
        self.env.pop('DEEPSEEK_API_KEY', None)
        self.env.pop('CI', None)
        self.write_overlay()

    def write_overlay(self, override: Path | None = None) -> None:
        """Rewrite the profile overlay the CLI loads over the built patch.

        @param override - a replay override file staging the next model response.
        """
        replay = {'id': 'tui-replay', 'name': str(ROOT / 'packages/test-support/llm-replay/lib/index.js'),
                  'config': {'file': str(FIXTURE), 'providers': [{'id': 'deepseek-official', 'models': [
                      {'id': 'deepseek-v4-flash', 'contextWindow': 128000},
                      {'id': 'tui-picked-model', 'contextWindow': 128000,
                       'reasoningEfforts': ['low', 'high'], 'defaultReasoningEffort': 'low'}]}]}}
        if override is not None:
            replay['config']['overrideFile'] = str(override)
        patches = [
            {'id': 'session-title-llm', 'disabled': True},
            {'id': 'agent-default-model', 'config': {'provider': 'deepseek-official', 'model': 'deepseek-v4-flash'}},
            {'id': 'session-persistence-jsonl', 'config': {'root': str(self.sessions_root), 'compression': 'none'}},
        ]
        if not self.live:
            patches = [{'id': 'llm-deepseek', 'disabled': True}, {'id': 'llm-pi-ai', 'disabled': True},
                       *patches, {'insert': [replay]}]
        self.overlay.write_text(json.dumps(patches))

    def command(self, *extra: str) -> list[str]:
        """Build the CLI invocation for one terminal.

        @param extra - arguments appended after the profile patches.
        @returns the argv to spawn.
        """
        return [self.node, *(['--env-file=' + str(ROOT / '.env')] if self.live else []),
                str(ROOT / 'apps/cli/lib/bin.js'), '--profile', 'tui',
                '--patch', str(ROOT / 'tui/packages/app/cordis.built.patch.yml'),
                '--patch', str(self.overlay), *extra]

    def logs(self) -> set[Path]:
        """Every persisted session log.

        @returns the set of log paths currently on disk.
        """
        return set(self.sessions_root.rglob('session.v*.jsonl'))

    @contextmanager
    def terminal(self, label: str, *extra: str):
        """Open a terminal, hand it to the caller, and verify its teardown.

        @param label - the scenario name, used for the trace and the transcript file.
        @param extra - extra CLI arguments, such as `--resume`.
        """
        terminal = Terminal(label, self.command(*extra), self.workspace, self.env, self.options)
        try:
            terminal.ready()
            yield terminal
            self.state[f'{label}-text'] = terminal.quit()
        except BaseException:
            terminal.save()
            raise
        finally:
            terminal.close()


@scenario('login, model and effort selection, paste, cursor editing, a bash tool turn, and the context estimate')
def fresh(run: Run) -> None:
    before = run.logs()
    prompt = run.prompt
    with run.terminal('fresh') as tty:
        tty.send(b'/lo', 'a partial slash command')
        tty.expect('› /login')
        tty.send(b'\t', 'Tab to complete it')
        tty.expect('> /login ▌')
        tty.send(b'\r', 'Enter')
        tty.expect('/login DEEPSEEK_API_KEY')
        if not run.live:
            tty.send(b'/model\r')
            tty.expect('Choose model')
            tty.send(b'tui-picked-model')
            tty.expect('› deepseek-official/tui-picked-model')
            tty.send(b'\r', 'Enter to pick the model')
            tty.expect('Choose reasoning effort: deepseek-official/tui-picked-model')
            tty.send(b'high')
            tty.expect('› high')
            tty.send(b'\r', 'Enter to pick the effort')
            tty.expect('Model set for the next turn: deepseek-official/tui-picked-model (high)')
        tty.send(b'\x1b[200~X' + prompt.encode() + b'Z\x1b[201~', 'a bracketed paste padded with X and Z')
        tty.expect('> X' + prompt + 'Z▌')
        tty.send(b'\x1b[H', 'Home')
        tty.expect('> ▌X' + prompt)
        tty.send(b'\x1b[3~', 'Delete, dropping the leading X')
        tty.expect('> ▌' + prompt + 'Z')
        tty.send(b'\x1b[F', 'End')
        tty.expect('> ' + prompt + 'Z▌')
        tty.send(b'\x7f', 'Backspace, dropping the trailing Z')
        tty.expect('> ' + prompt + '▌')
        tty.send(b'\x1b[D', 'Left')
        tty.expect('> ' + prompt[:-1] + '▌' + prompt[-1])
        tty.refuse('editing submitted the prompt without Enter', BASH_ROW in tty.text)
        tty.send(b'\r', 'Enter to submit')
        tty.wait("the bash result and the model's DONE line",
                 lambda text: '← TERMINAL_OK' in text and re.search(r'\n  DONE\r?\n', text) is not None)
        tty.follows('○ Ready', '← TERMINAL_OK')
        tty.expect('Context: ~')

    created = run.logs() - before
    assert len(created) == 1, f'expected one persisted session, got {sorted(created)}'
    path = created.pop()
    log = events(path)
    assert log[0]['agentPreset'] == 'standard'
    assert [e['data']['content'] for e in log
            if e['type'] == 'user/message' and e['data']['source']['kind'] == 'user'] \
        == [[{'type': 'text', 'text': run.prompt}]], 'cursor editing changed the submitted prompt'
    headers = [e['data']['header'] for e in log if e['type'] == 'request/header']
    if not run.live:
        assert headers and all(h['config']['model'] == 'tui-picked-model' and h['config']['reasoningEffort'] == 'high'
                               for h in headers), 'picker selection did not reach recorded requests'
    model = [e['data']['message']['content'] for e in log if e['type'] == 'assistant/message']
    expected_model = [e['data']['message']['content'] for e in run.recorded if e['type'] == 'assistant/message']
    assert (any(block.get('text') == 'DONE' for blocks in model for block in blocks if block['type'] == 'text')
            if run.live else model == expected_model), 'recorded model output changed or replay was not fully consumed'
    results = [e['data']['message']['content'] for e in log
               if e['type'] == 'tool/result' and e['surfaceOp'] == 'append']
    expected_results = [e['data']['message']['content'] for e in run.recorded
                        if e['type'] == 'tool/result' and e['surfaceOp'] == 'append']
    def result_text(groups):
        return [[(block.get('content'), block.get('isError', False)) for block in blocks] for blocks in groups]
    assert result_text(results) == result_text(expected_results), 'real tool output disagrees with the recording'
    run.state.update(log=path, id=log[0]['id'], model=model, headers=headers)


@scenario('exact replay of committed history, the restored draft, and history recall', requires=('fresh',))
def resume(run: Run) -> None:
    with run.terminal('resume', '--resume', run.state['id']) as tty:
        tty.expect('← TERMINAL_OK', '○ Ready')
        if not run.live:
            tty.expect('deepseek-official/tui-picked-model (high)')
        tty.send(b'Unsent draft')
        tty.expect('> Unsent draft▌')
        tty.send(b'\x1b[D', 'Left')
        tty.expect('> Unsent draf▌t')
        tty.send(b'\x1b[A', 'Up, recalling the submitted prompt')
        tty.expect('> ' + run.prompt + '▌')
        restored = tty.mark()
        tty.send(b'\x1b[B', 'Down, restoring the draft and its cursor')
        tty.expect('> Unsent draf▌t', after=restored)
        text = tty.text
    assert len(re.findall(r'\n  DONE\r?\n', text)) == 1, 'resume duplicated or omitted committed output'
    assert len([e for e in events(run.state['log']) if e['type'] == 'assistant/message']) \
        == len(run.state['model']), 'resume made an unsolicited model call'


@scenario('session picker cancellation, a new session, and switching back to committed history',
          requires=('fresh',), replay_only=True)
def navigate(run: Run) -> None:
    before = run.logs()
    identity = run.state['id']
    with run.terminal('navigate', '--resume', identity) as tty:
        start = tty.mark()
        tty.send(b'/sessions\r')
        tty.expect('Choose session', after=start)
        tty.send(b'\x1b', 'Escape to cancel the picker')
        tty.expect('Session navigation cancelled', after=start)

        start = tty.mark()
        tty.send(b'/sessions\r')
        tty.expect('Choose session', after=start)
        tty.send(b'New session')
        tty.expect('› New session', after=start)
        tty.send(b'\r', 'Enter to start a new session')
        new_id = tty.search(r'· Session: (session-[a-f0-9-]+)', after=start).group(1)
        assert new_id != identity, 'new-session selection reused the current identity'
        tty.expect('○ Ready  deepseek-official/deepseek-v4-flash', after=start)

        start = tty.mark()
        tty.send(b'/sessions\r')
        tty.expect('Choose session', after=start)
        tty.send(identity.encode(), 'the original session id')
        tty.expect('> ' + identity + '▌', after=start)
        tty.send(b'\r', 'Enter to switch back')
        tty.expect('← TERMINAL_OK', 'deepseek-official/tui-picked-model (high)', after=start)
        tty.follows('○ Ready', '← TERMINAL_OK')

        start = tty.mark()
        tty.send(b'\x1b[A', 'Up, recalling the last command')
        tty.expect('> /sessions▌', after=start)
        tty.send(b'\x1b[B', 'Down, back to an empty composer')
        tty.expect('> ▌', after=start)
        text = tty.text

    assert len(re.findall(r'\n  DONE\r?\n', text)) == 2, \
        'navigation did not replay the selected history exactly once per visit'
    log = events(run.state['log'])
    assert [e['data']['message']['content'] for e in log
            if e['type'] == 'assistant/message'] == run.state['model'], 'navigation changed model history'
    assert len([e for e in log if e['type'] == 'request/header']) == len(run.state['headers']), \
        'navigation requested a model response'
    for path in run.logs() - before:
        saved = events(path)
        assert not any(e['type'] == 'user/message' for e in saved), 'old draft or input crossed into the new session'
        runs = [e['data']['commandId'] for e in saved if e['type'] == 'command/run']
        settled = [e['data']['commandId'] for e in saved if e['type'] == 'command/done']
        assert runs == settled, 'navigation disposed a session before command settlement'


@scenario('skill and quoted-file completion, steering a running turn, interruption, and discarding queued input',
          replay_only=True)
def cancel(run: Run) -> None:
    skill = run.workspace / '.agents/skills/tui-smoke/SKILL.md'
    skill.parent.mkdir(parents=True, exist_ok=True)
    skill.write_text('---\nname: tui-smoke\ndescription: Terminal skill smoke\n'
                     'disable-model-invocation: true\n---\n\nTUI_SKILL_INSTRUCTIONS\n')
    referenced = run.workspace / 'notes folder/read me.txt'
    referenced.parent.mkdir(exist_ok=True)
    referenced.write_text('TUI_FILE_CONTENT_MUST_NOT_BE_INJECTED')
    # The replay hangs on this turn so the composer stays live while the agent runs.
    ready = run.root / 'stream-ready'
    override = run.root / 'cancel.json'
    override.write_text(json.dumps([{'kind': 'hang', 'readyFile': str(ready)}]))
    run.write_overlay(override)

    before = run.logs()
    with run.terminal('cancel') as tty:
        tty.send(b'/tui-sm', 'a partial skill command')
        tty.expect('› /tui-smoke', 'Terminal skill smoke')
        tty.send(b'\t', 'Tab to complete it')
        tty.expect('> /tui-smoke ▌')
        tty.refuse('completion submitting the skill before Enter', ready.exists())
        tty.send(b'Pause for a new direction about @notes', 'a file mention')
        tty.expect('› @"notes folder/')
        tty.send(b'\t', 'Tab to complete the directory')
        tty.expect('› @"notes folder/read me.txt"')
        tty.send(b'\t', 'Tab to complete the file')
        tty.expect('> /tui-smoke Pause for a new direction about @"notes folder/read me.txt" ▌')
        tty.refuse('file completion starting a model request before Enter', ready.exists())
        tty.send(b'\r', 'Enter to submit')
        tty.wait('the turn to start streaming and hang', lambda text: ready.exists() and 'partial' in text)
        tty.send(b'Discard this steering\r', 'steering typed into a running turn')
        tty.expect('Next step: Discard this steering', '/clear-pending discards queued input')
        tty.send(b'\x1b', 'Escape to interrupt the turn')
        tty.follows('○ Ready', 'Interrupted')
        tty.send(b'/clear-pending\r')
        tty.expect('Queued input discarded')

    created = run.logs() - before
    assert len(created) == 1, f'expected one cancellation session, got {sorted(created)}'
    path = created.pop()
    log = events(path)
    assert any(e['type'] == 'user/message' and e['data']['source'].get('kind') == 'skill-invocation'
               and e['data']['source'].get('name') == 'tui-smoke'
               and 'TUI_SKILL_INSTRUCTIONS' in json.dumps(e['data']['content'])
               for e in log), 'Harness did not log the selected skill instructions'
    assert any(e['type'] == 'user/message' and e['data']['source'].get('kind') == 'user'
               and e['data']['content'] == [{'type': 'text',
                                             'text': '/tui-smoke Pause for a new direction about '
                                                     '@"notes folder/read me.txt" '}]
               for e in log), 'completed file mention was not submitted literally'
    assert 'TUI_FILE_CONTENT_MUST_NOT_BE_INJECTED' not in json.dumps(log), 'file completion injected file contents'
    assert any(e['type'] == 'agent/inbox/spliced' and e['data'].get('outcome') == 'canceled'
               and e['data'].get('removedCount') == 1 for e in log), 'discard did not persist an inbox removal'
    run.state.update(cancelled_log=path, cancelled_id=log[0]['id'])


@scenario('a resumed session showing the discard and neither the discarded steering nor stale pending input',
          requires=('cancel',), replay_only=True)
def resume_cleared(run: Run) -> None:
    with run.terminal('resume-cleared', '--resume', run.state['cancelled_id']) as tty:
        tty.expect('Queued input discarded', '○ Ready', '@"notes folder/read me.txt"')
        text = tty.text
    assert 'Discard this steering' not in text, 'discarded steering returned after resume'
    assert '/clear-pending discards queued input' not in text, 'resume shows stale pending input'
    assert len([e for e in events(run.state['cancelled_log'])
                if e['type'] == 'turn/start']) == 1, 'resume drove discarded input'


@scenario('attachment staging, exact stored bytes, recorded model output, and metadata on resume', replay_only=True)
def attachments(run: Run) -> None:
    run.write_overlay()
    before = run.logs()
    name = 'notes with spaces.bin'
    data = b'TUI_ATTACHMENT_BYTES\x00\xff'
    (run.workspace / name).write_bytes(data)
    with run.terminal('attachments') as tty:
        tty.send(('/attach ' + name + '\r').encode())
        tty.expect('Staged attachments: 1', name + ' · ')
        start = tty.mark()
        tty.send(b'/sessions\r')
        tty.expect('Send or clear staged attachments before switching sessions', after=start)
        tty.send(b'\x1b[200~' + run.prompt.encode() + b'\x1b[201~')
        tty.expect('> ' + run.prompt + '▌')
        tty.send(b'\r', 'Enter to submit text and the staged file')
        tty.wait('the recorded reply after attachment admission',
                 lambda text: '← TERMINAL_OK' in text and re.search(r'\n  DONE\r?\n', text) is not None)
        tty.follows('○ Ready', '← TERMINAL_OK')

    created = run.logs() - before
    assert len(created) == 1, f'expected one attachment session, got {sorted(created)}'
    path = created.pop()
    log = events(path)
    users = [e['data']['content'] for e in log
             if e['type'] == 'user/message' and e['data']['source']['kind'] == 'user']
    assert len(users) == 1 and users[0][0] == {'type': 'text', 'text': run.prompt}, 'attachment submission changed prompt text'
    assert len(users[0]) == 2 and users[0][1]['type'] == 'file', 'attachment was not logged beside the prompt'
    attachment = users[0][1]['attachment']
    assert attachment['name'] == name and attachment['bytes'] == len(data), 'file metadata changed'
    stored = list(run.home.rglob(name))
    assert len(stored) == 1 and stored[0].read_bytes() == data, 'stored attachment bytes differ from the source'
    model = [e['data']['message']['content'] for e in log if e['type'] == 'assistant/message']
    expected = [e['data']['message']['content'] for e in run.recorded if e['type'] == 'assistant/message']
    assert model == expected, 'attachment flow did not consume the recorded model output'
    with run.terminal('attachments-resume', '--resume', log[0]['id']) as tty:
        tty.expect('← TERMINAL_OK', name + ' · ' + str(len(data)) + ' B')
        tty.refuse('sent attachments returning as a draft', 'Staged attachments:' in tty.text)
    assert [e['data']['message']['content'] for e in events(path) if e['type'] == 'assistant/message'] == model, 'resume made an unsolicited model call'


def selected(names: list[str] | None, live: bool) -> list[Scenario]:
    """Resolve the scenarios to run, in declaration order, with prerequisites.

    @param names - the names asked for, or None for all of them.
    @param live - whether the run uses the real API, which skips replay-only scenarios.
    @returns the scenarios to run, each preceded by what it requires.
    """
    if names:
        unknown = [name for name in names if name not in SCENARIOS]
        if unknown:
            raise SystemExit(f'unknown scenario(s): {", ".join(unknown)}\n'
                             f'known: {", ".join(SCENARIOS)}')
        wanted: set[str] = set()
        pending = list(names)
        while pending:
            name = pending.pop()
            if name in wanted:
                continue
            wanted.add(name)
            pending.extend(SCENARIOS[name].requires)
    else:
        wanted = set(SCENARIOS)
    chosen = [SCENARIOS[name] for name in SCENARIOS if name in wanted]
    if live:
        skipped = [item.name for item in chosen if item.replay_only]
        if names and skipped:
            raise SystemExit(f'--live cannot run replay-only scenario(s): {", ".join(skipped)}')
        chosen = [item for item in chosen if not item.replay_only]
    return chosen


def main() -> None:
    """Parse arguments, run the selected scenarios, and report each one."""
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('--live', action='store_true',
                        help='use the DeepSeek key in the root .env instead of recorded replay')
    parser.add_argument('--only', action='append', metavar='NAME',
                        help='run this scenario and its prerequisites; repeatable')
    parser.add_argument('--list', action='store_true', help='print the scenarios and exit')
    parser.add_argument('--trace', action='store_true', help='print each step to stderr as it is satisfied')
    parser.add_argument('--keep-home', action='store_true',
                        help='keep the temporary DSH_HOME and workspace for inspection')
    parser.add_argument('--step-timeout', type=float, default=30.0, metavar='SECONDS',
                        help='how long one step may take (default: 30)')
    parser.add_argument('--budget', type=float, default=120.0, metavar='SECONDS',
                        help='how long one terminal may take (default: 120)')
    parser.add_argument('node', nargs='?', default='node', help='the node binary to run (default: node)')
    args = parser.parse_args()

    if args.list:
        for item in SCENARIOS.values():
            marks = [*( ['requires ' + ', '.join(item.requires)] if item.requires else []),
                     *(['replay only'] if item.replay_only else [])]
            print(f'{item.name:<16} {item.summary}' + (f'\n{"":<16} ({"; ".join(marks)})' if marks else ''))
        return

    chosen = selected(args.only, args.live)
    options = Options(step=args.step_timeout, budget=args.budget, trace=args.trace)
    shutil.rmtree(options.artifacts, ignore_errors=True)
    mode = 'live API' if args.live else 'recorded replay'
    directory = Path(tempfile.mkdtemp(prefix='dsh-tui-pty-'))
    started = time.monotonic()
    failed, item = None, None
    try:
        run = Run(directory, args, options)
        for item in chosen:
            step = time.monotonic()
            item.body(run)
            print(f'PASS {item.name} ({time.monotonic() - step:.1f}s): {item.summary}')
        print(f'PASS {args.node} ({mode}): {len(chosen)} scenario(s) in {time.monotonic() - started:.1f}s')
    except AssertionError as error:
        # The message already carries the step, the process state, and the
        # screen; a traceback through the wait helpers would bury it.
        failed = error
        print(f'\nFAIL {item.name if item else "setup"}: {error}', file=sys.stderr)
    finally:
        if failed is not None or args.keep_home:
            print(f'\nsession logs and workspace kept at {directory}', file=sys.stderr)
        else:
            shutil.rmtree(directory, ignore_errors=True)
    if failed is not None:
        raise SystemExit(1)


if __name__ == '__main__':
    main()
