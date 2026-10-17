# SQLite request diagnostics

Uruk records incoming **API** requests and every outgoing **model HTTP attempt**
in `request_logs` inside the project's `.uruk/state.sqlite`. This includes health,
unknown routes, method errors, validation failures, provider errors, and retries.
Frontend HTML/assets and source-retrieval/search HTTP traffic are not covered by
this request table. Existing task failures, budget facts, and run stop conditions
remain in their own tables; an HTTP `success` does not mean scientific work passed.

The table is operator-only: there is no public log endpoint. It contains research
prompts and model replies, so protect the SQLite database and its backups like
private research data. Resolve the project's path from the running service's
`uruk --project <path>` argument; diagnostics live at `<path>/.uruk/state.sqlite`.

## What is stored

- A unique physical request `id`, incoming `request_id` (matching the response's
  `x-request-id`), and model `logical_id` grouping its retries.
- `run_id` and `task_id` when known. Start responses populate their newly-created
  `run_id`; subsequent API requests use the route's run id. Provider attempts use
  the runtime context. Calls made without a task have a null task id.
- Attempt ordinal, UTC start/finish timestamps, elapsed milliseconds, status,
  outcome, raw internal error/source chain, and actual recovery-gate retry delay.
- Request method, sanitized URL, model parameters and messages, safe headers,
  response headers/body, upstream request ids, and byte/capture metadata.

Each start commits **before** its handler or model HTTP send is dispatched. A
normal result commits before the response stream reaches EOF or the model result
returns. Diagnostic writes use short transactions admitted through the project's
single-writer lock, with the existing SQLite busy and pool bounds; no database
transaction spans a network wait. A zero API deadline refuses the request before
polling its handler, so already-expired requests cannot initialize writes. If a start cannot
be persisted, the operation is not dispatched. Finalization errors are reported,
not silently dropped. Journald remains the fallback for failures writing SQLite
itself: a full/unavailable database cannot record its own write failure.

## Outcomes and interruptions

`outcome` is `in_progress`, `success`, `http_error`, `retryable`, `permanent_error`,
`stream_error`, `cancelled`, or `interrupted`.

- Every physical provider retry is a separate row, not an overwrite of the first
  failed attempt. `logical_id` connects those attempts; `retry_delay_ms` records
  the gate's remaining delay when that failure schedules its next probe.
- Dropping a request future or response stream schedules a cancellation update,
  including available partial capture. This cannot prove the provider stopped
  computing; remote outcome/cost may remain unknown.
- A hard process kill can prevent the cancellation update. Its already-committed
  start remains `in_progress`; after acquiring the exclusive project lock, server
  startup marks such leftovers `interrupted`, not successful. Bytes that had not
  been checkpointed before the crash cannot be recovered.
- Live SSE streams remain `in_progress` until closed, with their status recorded
  once headers are ready. They are not buffered waiting for the entire stream.

## Sensitive data and bounded capture

Authorization, cookies, API keys, sensitive query/JSON fields, and known credential
values are redacted. Header names are retained but only explicitly safe values
are logged. Token-usage fields such as `prompt_tokens` and `max_tokens` are retained.
Arbitrary secrets embedded in prose cannot all be recognized automatically;
operator access restrictions remain essential.

Capture is bounded to **1 MiB per body**. Full smaller textual/JSON bodies are
stored. Binary bodies, oversized bodies, and malformed JSON containing credential
fields have explicit omission metadata instead of an unsafe raw dump. Byte count,
completeness, truncation, and read errors remain visible. Inbound capture follows
bytes the handler consumes; a rejected/unread body is marked incomplete, not
represented as a fully received empty body. Response capture happens before gzip.

For non-success provider statuses, optional diagnostic bodies get at most **100
milliseconds** and 1 MiB of reading. If the body stalls, the known HTTP status
still determines recovery/permanent refusal and the missing body has an explicit
read-error marker. Success-body reads retain the configured completion timeout.

No automatic deletion is enabled. Monitor database/disk growth, particularly when
health checks are frequent, and choose an archival/retention policy explicitly.
Never delete rows for live requests. Normal SQLite WAL/backup procedures apply.

## Operator queries

Latest errors and retries:

```sql
SELECT id, request_id, direction, run_id, task_id, attempt,
       started_at, duration_ms, status, outcome, retry_delay_ms, error
FROM request_logs
WHERE outcome NOT IN ('success', 'in_progress')
ORDER BY started_at DESC LIMIT 100;
```

Full request and response for one physical attempt (bind its id):

```sql
SELECT request, response, error FROM request_logs WHERE id = ?;
```

One run, including every provider resend:

```sql
SELECT logical_id, attempt, started_at, status, outcome,
       retry_delay_ms, error, response
FROM request_logs WHERE run_id = ? ORDER BY started_at, id;
```

Currently open requests:

```sql
SELECT id, direction, run_id, started_at, status, request, response
FROM request_logs WHERE outcome = 'in_progress' ORDER BY started_at;
```

Run these as the service owner, using a read-only SQLite connection for inspection.
Do not paste full prompt/reply rows into public issues without reviewing them.
