"""Run in the workspace: the log is untouched and the answer holds the count and the timestamp."""

import os
import re
import sys

sys.path.insert(0, os.environ["BAKE_EVAL_TASK_DIR"])
import gen_log

with open("app.log", encoding="utf-8") as handle:
    if handle.read() != "\n".join(gen_log.lines()) + "\n":
        sys.exit("app.log was changed")
with open(os.environ["BAKE_EVAL_ANSWER"], encoding="utf-8") as handle:
    answer = handle.read()
count, first = gen_log.expected()
if not re.search(rf"(?<![\d.,]){count}(?![\d.,]*\d)", answer):
    sys.exit(f"the answer does not give the count {count}")
if first not in answer:
    sys.exit(f"the answer does not give the timestamp {first}")
