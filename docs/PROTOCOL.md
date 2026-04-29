# Protocol Notes

The initial runtime uses WebSocket transport with JSON messages.

Workers connect to the coordinator and first send:

```json
{ "kind": "worker", "data": { "type": "register", "data": { "...": "..." } } }
```

CLI clients connect to the same endpoint and first send:

```json
{ "kind": "client_request", "data": { "type": "list_nodes" } }
```

The protocol types live in `crates/protocol`.
