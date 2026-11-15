# Service log

`app.log` holds one event a line:

```
<timestamp> <LEVEL> <service> <message> [key=value ...]
```

- `timestamp` is UTC, `YYYY-MM-DDTHH:MM:SSZ`.
- `LEVEL` is `INFO`, `WARN`, or `ERROR`.
- An `ERROR` line carries `code=<code>`. Other lines may mention a code in their message.
