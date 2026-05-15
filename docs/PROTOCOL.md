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
job_status
create_lease
list_leases
release_lease
diagnose_job
```

`run_process` streams `run_started`, task lifecycle events, log lines, and
`job_finished` when `wait=true`. When `wait=false`, the coordinator returns
`run_started` followed by `ack`; the worker task continues running in the
background.

`run_process` also accepts optional per-node environment overrides, an optional
lease id, and an optional timeout. When a lease id is present and no explicit nodes are supplied,
the coordinator runs on the lease nodes. When both are present, the explicit node
list must be a subset of the lease. Timeout enforcement happens on the worker;
when the limit is reached, the worker logs a system line and terminates the task
process group.

Leases are current-process reservations used by ML-oriented workflows to avoid
accidentally starting overlapping work on the same node. The initial lease model
tracks node membership, optional GPUs per node, and whether the lease is
exclusive; it is not yet persisted across coordinator restarts.

The protocol types live in `crates/protocol`.
