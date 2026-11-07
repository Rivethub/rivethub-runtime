# RivetHub systemd commands

`rh service init|start|stop|restart|status|logs` implements B9's service contract in
`crates/testkit/tests/fixtures/cli/README.md`. Effective UID selects system mode
(`/etc/systemd/system/rivethub.service`) or user mode
(`~/.config/systemd/user/rivethub.service`). Init generates the unit from `service::Unit`
using this executable's absolute path and the configured state directory's `.env`.
It publishes the file exclusively, reports user lingering, and refuses existing
files, traversal and directory links escaping the home directory. Systemd quoting
preserves spaces, quotes, Unicode, dollar signs and percent signs literally.

Start and restart reload the manager, execute the action and show status. A failed
reload or action stops the sequence and returns failure with stderr. Status retains
the child exit code. Logs inherits the terminal streams and follows the journal until
interrupted; cancellation kills and reaps the child before returning success. All
commands use `tokio::process`; blocking account lookup and unit publication run on
`spawn_blocking`.

The fixture directory currently contains the written contract only, with no captured
CLI transcripts. The `service` acceptance tests exercise every service row using
fake `systemctl`, `journalctl` and `loginctl` executables in a temporary PATH. They
check both scopes, unit serialization, overwrite refusal, path containment, quoting,
status codes, subprocess errors and event-driven cancellation. No service scenario is
skipped; the existing checked deferred list covers the remaining CLI commands in
that same fixture README. `rh start` supervises `rh gateway`, `rh worker --role`
for the roles the configuration switches on, and `rh mcp` when its network
transport is configured. It holds the state-directory pid lock, restarts children
with backoff, and forwards SIGTERM. `Config::load` with `Profile::Node` and
`rh config validate` still require `runtime`, `agents`, and `providers`. `rh local`
does not invent those values. With no resolved file it writes the SQLite path and
the gateway bind, loads that file through `Config::load` with `Profile::Standalone`,
migrates it, and claims the state directory.
