#!/usr/bin/env python3
"""Keyless built-profile replay and resume through a real POSIX terminal."""
import argparse
import json
import os
from pathlib import Path
import re
import select
import signal
import struct
import subprocess
import tempfile
import termios
import time
import fcntl

ROOT = Path(__file__).resolve().parents[2]
FIXTURE = ROOT / 'snapshots/session/bash-tool-turn/session.v3.jsonl'
ANSI = re.compile(r'\x1b\][^\x07]*(?:\x07|\x1b\\)|\x1b\[[0-?]*[ -/]*[@-~]')


def events(path):
    return [json.loads(line) for line in path.read_text().splitlines() if line]


def run_terminal(command, cwd, env, drive):
    master, slave = os.openpty()
    before = termios.tcgetattr(slave)
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 40, 120, 0, 0))
    child = subprocess.Popen(command, cwd=cwd, env=env, stdin=slave, stdout=slave,
                             stderr=slave, start_new_session=True)
    output = bytearray()
    deadline = time.monotonic() + 90

    def read_until(predicate):
        while not predicate(ANSI.sub('', output.decode(errors='replace'))):
            if b'failed to import' in output:
                raise AssertionError('profile plugin import failed')
            if time.monotonic() >= deadline:
                raise AssertionError('terminal condition timed out')
            ready, _, _ = select.select([master], [], [], 0.1)
            if ready:
                output.extend(os.read(master, 65536))
            elif child.poll() is not None:
                raise AssertionError(f'child exited before terminal condition: {child.returncode}')

    try:
        read_until(lambda text: '○ Ready' in text and b'\x1b[?2004h' in output)
        drive(master, read_until, output)
        os.write(master, b'\x03')
        read_until(lambda text: 'Press Ctrl-C again' in text)
        os.write(master, b'\x03')
        while child.poll() is None:
            if time.monotonic() >= deadline:
                raise AssertionError('quit did not finish')
            ready, _, _ = select.select([master], [], [], 0.1)
            if ready:
                output.extend(os.read(master, 65536))
        while select.select([master], [], [], 0)[0]:
            output.extend(os.read(master, 65536))
        assert child.returncode == 0, f'child exited {child.returncode}'
        assert termios.tcgetattr(slave) == before, 'terminal modes were not restored'
        assert b'\x1b[?2004h' in output and b'\x1b[?2004l' in output, 'paste mode was not released'
        return ANSI.sub('', output.decode(errors='replace'))
    except BaseException:
        print(ANSI.sub('', output.decode(errors='replace'))[-10000:])
        raise
    finally:
        if child.poll() is None:
            os.killpg(child.pid, signal.SIGKILL)
            child.wait()
        os.close(master)
        os.close(slave)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--live', action='store_true', help='Use the DeepSeek key in the root .env instead of recorded replay')
    parser.add_argument('node', nargs='?', default='node')
    args = parser.parse_args()
    recorded = events(FIXTURE)
    prompt = next(e['data']['content'][0]['text'] for e in recorded
                  if e['type'] == 'user/message' and e['data']['source']['kind'] == 'user')
    with tempfile.TemporaryDirectory(prefix='dsh-tui-pty-') as directory:
        root = Path(directory)
        home = root / 'home'
        workspace = root / 'workspace'
        workspace.mkdir()
        profile = home / 'profiles/tui'
        profile.mkdir(parents=True)
        (profile / 'package.json').write_text(json.dumps({
            'name': 'tui-replay-profile', 'private': True,
            'dsh': {'profile': {'bundles': ['@deepseek-ai/dsh-base', '@deepseek-ai/dsh-headless']}},
        }))
        overlay = root / 'replay.patch.yml'
        patches = [
            {'id': 'llm-deepseek', 'disabled': True},
            {'id': 'llm-pi-ai', 'disabled': True},
            {'id': 'session-title-llm', 'disabled': True},
            {'id': 'agent-default-model', 'config': {'provider': 'deepseek-official', 'model': 'deepseek-v4-flash'}},
            {'id': 'session-persistence-jsonl', 'config': {'root': str(home / 'sessions'), 'compression': 'none'}},
            {'insert': [{'id': 'tui-replay', 'name': str(ROOT / 'packages/test-support/llm-replay/lib/index.js'),
                         'config': {'file': str(FIXTURE), 'providers': [{'id': 'deepseek-official', 'models': [{'id': 'deepseek-v4-flash', 'contextWindow': 128000}]}]}}]},
        ]
        if args.live:
            patches = [patch for patch in patches if patch.get('id') not in ['llm-deepseek', 'llm-pi-ai'] and 'insert' not in patch]
        overlay.write_text(json.dumps(patches))
        command = [args.node, *(['--env-file=' + str(ROOT / '.env')] if args.live else []), str(ROOT / 'apps/cli/lib/bin.js'), '--profile', 'tui',
                   '--patch', str(ROOT / 'tui/packages/app/cordis.built.patch.yml'), '--patch', str(overlay)]
        env = {**os.environ, 'DSH_HOME': str(home), 'TERM': 'xterm-256color', 'NO_COLOR': '1'}
        env.pop('DEEPSEEK_API_KEY', None)
        env.pop('CI', None)

        def fresh(master, wait, output):
            os.write(master, b'/login\r')
            wait(lambda text: '/login DEEPSEEK_API_KEY' in text)
            os.write(master, b'\x1b[200~' + prompt.encode() + b'\x1b[201~')
            wait(lambda text: prompt in text)
            assert b'\xe2\x9a\x99 bash' not in output, 'paste submitted without Enter'
            os.write(master, b'\r')
            wait(lambda text: '← TERMINAL_OK' in text and re.search(r'\n  DONE\r?\n', text) is not None)
            wait(lambda text: text.rfind('○ Ready') > text.rfind('← TERMINAL_OK'))

        run_terminal(command, workspace, env, fresh)
        logs = list((home / 'sessions').rglob('session.v*.jsonl'))
        assert len(logs) == 1, f'expected one persisted session, got {logs}'
        log = events(logs[0])
        assert log[0]['agentPreset'] == 'standard'
        model = [e['data']['message']['content'] for e in log if e['type'] == 'assistant/message']
        expected_model = [e['data']['message']['content'] for e in recorded if e['type'] == 'assistant/message']
        assert (any(block.get('text') == 'DONE' for blocks in model for block in blocks if block['type'] == 'text') if args.live else model == expected_model), 'recorded model output changed or replay was not fully consumed'
        results = [e['data']['message']['content'] for e in log if e['type'] == 'tool/result' and e['surfaceOp'] == 'append']
        expected_results = [e['data']['message']['content'] for e in recorded if e['type'] == 'tool/result' and e['surfaceOp'] == 'append']
        result_text = lambda groups: [[(block.get('content'), block.get('isError', False)) for block in blocks] for blocks in groups]
        assert result_text(results) == result_text(expected_results), 'real tool output disagrees with the recording'

        def resumed(master, wait, output):
            wait(lambda text: '← TERMINAL_OK' in text and '○ Ready' in text)

        text = run_terminal(command + ['--resume', log[0]['id']], workspace, env, resumed)
        assert len(re.findall(r'\n  DONE\r?\n', text)) == 1, 'resume duplicated or omitted committed output'
        assert len([e for e in events(logs[0]) if e['type'] == 'assistant/message']) == len(model), 'resume made an unsolicited model call'
        print(f'PASS {args.node} ({"live API" if args.live else "recorded replay"}): bash output, login, paste, exact resume, exit and terminal restoration')


if __name__ == '__main__':
    main()
