"""The states of a job."""

PENDING = "pending"
RUNNING = "running"
RETRYING = "retrying"
DONE = "done"
FAILED = "failed"
CANCELLED = "cancelled"

# The states a job never leaves once it reaches them.
FINISHED_STATES = frozenset({DONE, FAILED, CANCELLED})
