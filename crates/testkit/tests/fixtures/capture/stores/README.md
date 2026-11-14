# Harness transcript store fixtures

These files are the replay inputs for the capture store readers. They are not copies of a recorded harness session. The inventory's store section names paths and, for Hermes, a `messages` query. It does not include sample records for Codex, Grok, Hermes, Kimi, OpenCode, or Cursor. Each file below is a typed example built from that path description so the reader has something to parse. Claude Code is not duplicated here; its reader replays `capture/claude-hook/transcript.jsonl`.

`codex-rollout.jsonl` is one Codex rollout. A user turn comes from `event_msg` / `user_message`, or from `event_msg` / `item_completed` with a `UserMessage` item (newer Codex builds write only this form). An assistant turn comes from `response_item` / `message`. A `function_call` is paired with `function_call_output` into one tool message on the output line; `arguments` is a JSON string. `session_meta`, `agent_message`, `token_count`, a user `response_item`, `turn_context`, and `compacted` are known non-turns. The filename in a store is `sessions/YYYY/MM/DD/rollout-<stamp>-<uuid>.jsonl`.

`grok-updates-open.jsonl` sits at `sessions/<encoded-cwd>/<uuid>/updates.jsonl`. Its lines follow the shape of real Grok Build `session/update` records with sanitized content: `params._meta` carries `agentTimestampMs` and, for turn events, `promptId`; `params.update.sessionUpdate` names the record. A web search result carries a `rawOutput` with no `type` and is kept as its JSON. The first turn is closed by `turn_completed`. The second turn has no `turn_completed` and the file ends in a partly written line, so the reader holds that turn back. `grok-updates-rest.jsonl` is the remainder of that line plus the closing `turn_completed`; appending it releases the held turn.

`kimi-wire.jsonl` and `kimi-index.jsonl` sit in `sessions/<work-dir>/session_<uuid>/`. The transcript is `agents/main/wire.jsonl` (`user`, `assistant`, `tool_call`, `tool_result`). `session_index.jsonl` records the session id and is not a turn. Live deltas are out of slice.

`omp.jsonl` follows the record shapes of a recorded omp 18.13.3 session with sanitized content. It sits at `sessions/<encoded-cwd>/<stamp>_<uuid>.jsonl` under the omp agent directory. The leading `title` line, the `session` header, `model_change`, `thinking_level_change` and `custom` records are not turns. Turns are `message` records: `user`, `assistant` text parts, and `toolResult` as a tool message named by `toolName`. The `status` line stands in for a record type the reader does not know.

`cursor.jsonl` is `projects/<slug>/agent-transcripts/<uuid>/<uuid>.jsonl`. The record tag is `role` (`user`, `assistant`, `tool`). Live turns are out of slice.

Hermes and OpenCode have no JSON fixture. Tests build a sqlite file with these tables:

```sql
CREATE TABLE messages (
    session_id TEXT NOT NULL,
    role TEXT NOT NULL,
    content TEXT NOT NULL,
    timestamp TEXT NOT NULL
);

CREATE TABLE message (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL,
    role TEXT NOT NULL,
    created_at TEXT NOT NULL
);

CREATE TABLE part (
    id TEXT PRIMARY KEY,
    message_id TEXT NOT NULL,
    session_id TEXT NOT NULL,
    type TEXT NOT NULL,
    text TEXT,
    tool_name TEXT,
    tool_arguments TEXT,
    tool_result TEXT,
    created_at TEXT NOT NULL
);
```

Hermes reads `state.db`: `rowid`, `session_id`, `role`, `content`, `timestamp` for `session_id` and `rowid` above the cursor, ordered by `timestamp`, then `rowid`. Role text is `user`, `assistant`, `tool`, or `system`. Anything else is an unrecognized speaker. The table has no tool columns, so a tool row is a tool-role message without a tool name.

OpenCode reads `opencode.db`. Parts are left-joined to messages. `type` `text` uses the message role. `type` `tool` is a tool message. Any other part type is an unknown record. A part with no message row is an unrecognized speaker.

`opencode-v2-subagent.jsonl` is one real OpenCode 2.3.0 session and the `explore` subagent it launched, read row by row from `opencode.db` (`session_v2` and `session_message`) with the directory, kernel string and model name replaced. The parent ran a shell command, read a file and used the `subagent` tool; the child's `session_v2.parent_id` names the parent. Each assistant message's `content[]` holds `reasoning`, `text` (including the `"\n\n"` items 2.3.0 writes before tool calls) and `tool` items in the order the model produced them. Tests load it into a database with the 2.3.0 table layout.
