"""The functions a job file can name."""

import threading
import time


def square(number):
    return int(number) ** 2


def add(*numbers):
    return sum(int(number) for number in numbers)


def echo(*words):
    return " ".join(words)


def pause(seconds):
    time.sleep(float(seconds))
    return f"slept {seconds}"


def fail(*words):
    raise ValueError(" ".join(words) or "failed")


_attempts = {}
_attempts_lock = threading.Lock()


def flaky(key, failures):
    """Fails the first `failures` calls with the same `key` in this process."""
    with _attempts_lock:
        _attempts[key] = _attempts.get(key, 0) + 1
        attempt = _attempts[key]
    if attempt <= int(failures):
        raise ConnectionError(f"attempt {attempt} of {key} failed")
    return f"ok after {attempt}"


TASKS = {
    "add": add,
    "echo": echo,
    "fail": fail,
    "flaky": flaky,
    "pause": pause,
    "square": square,
}
