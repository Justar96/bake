"""Request limits, overridable from the environment."""

import os


def max_upload_mb():
    """The largest upload accepted, in megabytes."""
    return int(os.environ.get("MAX_UPLOAD_MB", "25"))
