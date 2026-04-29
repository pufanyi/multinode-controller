# Protocol Notes

The initial runtime uses WebSocket transport with JSON messages.

When authentication is enabled, the WebSocket upgrade request must include:

```text
Authorization: Bearer <token>
X-Agent-Role: worker
```

or:

```text
Authorization: Bearer <token>
X-Agent-Role: client
```

The coordinator validates the role-specific token during the HTTP upgrade
handshake. It rejects unauthorized connections before reading protocol messages.
Tokens are configured from local files on startup.

Workers connect to the coordinator and first send:

```json
{ "kind": "worker", "data": { "type": "register", "data": { "...": "..." } } }
```

CLI clients connect to the same endpoint and first send:

```json
{ "kind": "client_request", "data": { "type": "list_nodes" } }
```

Current client request types include:

```text
list_nodes
run_process
list_jobs
tail_job
kill_job
```

`run_process` streams `run_started`, task lifecycle events, log lines, and
`job_finished` when `wait=true`. When `wait=false`, the coordinator returns
`run_started` followed by `ack`.

The protocol types live in `crates/protocol`.
