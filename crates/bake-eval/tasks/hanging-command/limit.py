"""Runs a command with a hard time limit: python3 limit.py SECONDS COMMAND...

The command runs in its own session with a marker in its environment. When
it ends or its time runs out, every process still carrying the marker is
killed, including ones that moved to a session of their own, and on a
timeout the exit status is 124.
"""

import os
import signal
import subprocess
import sys

MARKER = "BAKE_EVAL_LIMIT"


def kill_marked(token):
    needle = f"{MARKER}={token}".encode()
    for name in os.listdir("/proc"):
        if not name.isdigit() or int(name) == os.getpid():
            continue
        try:
            with open(f"/proc/{name}/environ", "rb") as handle:
                environ = handle.read().split(b"\0")
        except OSError:
            continue
        if needle in environ:
            try:
                os.kill(int(name), signal.SIGKILL)
            except OSError:
                pass


seconds = float(sys.argv[1])
command = sys.argv[2:]
token = f"{os.getpid()}-{os.urandom(6).hex()}"
env = dict(os.environ, **{MARKER: token})
process = subprocess.Popen(command, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                           start_new_session=True, env=env)
try:
    output, _ = process.communicate(timeout=seconds)
except subprocess.TimeoutExpired:
    os.killpg(process.pid, signal.SIGKILL)
    kill_marked(token)
    output, _ = process.communicate()
    sys.stdout.write(output.decode("utf-8", "replace")[-1500:])
    print(f"\n{' '.join(command)} did not finish within {seconds:g} s")
    sys.exit(124)
kill_marked(token)
sys.stdout.write(output.decode("utf-8", "replace"))
sys.exit(process.returncode)
