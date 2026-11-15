"""When and how soon a failed job is tried again."""


class RetryPolicy:
    """Up to `attempts` tries in all, for errors that are instances of
    `retry_on`, waiting `backoff`, then twice that, and so on, between them."""

    def __init__(self, attempts=1, retry_on=(Exception,), backoff=0.0):
        if attempts < 1:
            raise ValueError("a job needs at least one attempt")
        self.attempts = attempts
        self.retry_on = tuple(retry_on)
        self.backoff = backoff

    def should_retry(self, attempts, error):
        """Whether a job that has made `attempts` tries, the last failing with
        `error`, gets another."""
        return attempts < self.attempts and isinstance(error, self.retry_on)

    def delay(self, attempts):
        """The wait after the `attempts`-th failed try."""
        return self.backoff * 2 ** (attempts - 1)


NO_RETRY = RetryPolicy()
