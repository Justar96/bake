"""Release checks. Stops at the first failure."""

import configparser
import re
import sys
from pathlib import Path


def fail(message):
    print(f"FAIL: {message}")
    sys.exit(1)


def check_settings():
    parser = configparser.ConfigParser()
    parser.read("settings.ini")
    for option in ("host", "port"):
        if not parser.has_option("server", option):
            fail(f"settings.ini: [server] needs {option}")
    port = parser.get("server", "port")
    if not port.isdigit() or not 1 <= int(port) <= 65535:
        fail(f"settings.ini: [server] port must be a number from 1 to 65535, not {port!r}")


def check_limits():
    from app.limits import max_upload_mb

    value = max_upload_mb()
    if type(value) is not int:
        fail(f"app.limits.max_upload_mb() must return an int, not {type(value).__name__}")
    if value <= 0:
        fail("app.limits.max_upload_mb() must be positive")


def check_version():
    path = Path("VERSION")
    if not path.is_file():
        fail("the VERSION file is missing")
    text = path.read_text(encoding="utf-8").strip()
    if not re.fullmatch(r"\d+\.\d+\.\d+", text):
        fail(f"VERSION must be MAJOR.MINOR.PATCH, not {text!r}")
    from app import __version__

    if text != __version__:
        fail(f"VERSION says {text} but app.__version__ is {__version__}")


for check in (check_settings, check_limits, check_version):
    check()
print("OK: every release check passes")
