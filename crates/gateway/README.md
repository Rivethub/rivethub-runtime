# Gateway

The `gateway` crate is the HTTP API (CONTRACT B1, B17) and the services behind it: capture and memory, the stored
sessions and their transcripts and summary trees (`history`), topics, entities, review, Today, Tidy and the wiki,
tenancy (`tenancy`, `scope`), account and models, and the plug-in host (`plugins`: registry, supervisor, the Landlock
`launch`, the plug-in API socket, events, jobs, tools and the creator). `/api/events/ws` carries plug-in notices only.
Routes live under `routes/`. It calls kernel ports only; the `rivethub` composition root supplies the adapters.

Identity is a bearer token from the users file or a loopback caller, who is the node's owner. The listener's TLS
serves the gateway's own certificate and asks for no client certificate.
