# jobq

A small in-process job queue: worker threads run submitted functions, retry
the ones that fail according to a retry policy, and let callers wait for one
job or for all of them.

```python
from jobq import JobQueue, RetryPolicy

with JobQueue(workers=4) as jobs:
    job = jobs.submit(fetch, "https://example.com", retry=RetryPolicy(attempts=3))
    jobs.join()
    print(job.state, job.result, job.error)
```

A job is `pending` until a worker takes it, then `running`, and ends
`done`, `failed`, or `cancelled`; between attempts it is `retrying`.

The nightly maintenance runs `python3 -m jobq.cli examples/nightly.jobs`;
`jobq/batch.py` describes job files and `jobq/cli.py` the exit status.

## Tests

```sh
python3 -m unittest discover -s tests -t .
```
