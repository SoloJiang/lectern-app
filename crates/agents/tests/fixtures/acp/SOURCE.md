# Synthetic ACP scripts

Everything here is made up for the tests of the ACP client (`docs/design/agents.md`「ACP 客户端」,
ADR 0060). None of it comes from a real session log or a real adapter run; no file was copied from
`~/.claude`, `~/.codex` or any other user directory, and nothing names a model.

Each `*.jsonl` file is the script of the fake adapter `lectern-agents-fake-acp`
(`src/bin/fake_acp.rs`, which documents the format): one rule per line, `{"on": <method>, "do": [<actions>]}`.
The messages in the scripts follow the ACP v1 shapes that Lectern reads (`initialize`, `session/resume`,
`session/load`, `session/prompt`, `session/cancel`, `session/update`, `session/request_permission`), with
the fields the real adapters send for them (checked against their public sources, see the「核对」table in
the design). The unit test `acp::wire::tests::fixture_messages_parse` parses every message in these files.

| File | What it plays |
|---|---|
| `basic.jsonl` | `session/resume`; a turn with a plan, tool calls, text, a usage update and update kinds Lectern skips; a second turn |
| `load.jsonl` | an adapter without `session/resume`: `session/load` replays history before it answers |
| `permission.jsonl` | a command that needs the user's answer, with a duplicate option and all four kinds of options |
| `permission_deny.jsonl` | the same request; the call fails after the user rejects it |
| `permission_stubborn.jsonl` | the request waits in an adapter that ignores SIGTERM, so it can still read the answer Lectern writes when the connection closes |
| `permission_left_open.jsonl` | the turn ends (once with a result, once with an error) while a request is still open |
| `permission_cancel.jsonl` | the user stops the turn while the request waits |
| `permission_odd.jsonl` | requests Lectern answers `cancelled` itself, and an edit request |
| `version2.jsonl`, `no_restore.jsonl` | an adapter Lectern must not use |
| `auth.jsonl`, `load_error.jsonl` | `session/resume` fails |
| `prompt_error.jsonl`, `prompt_auth.jsonl` | `session/prompt` fails |
| `exit_on_initialize.jsonl`, `garbage.jsonl`, `oversize.jsonl` | the adapter dies, sends a line that is not JSON, sends a line over 16 MiB |
| `silent.jsonl`, `silent_restore.jsonl` | no answer to `initialize` / `session/resume` |
| `client_requests.jsonl` | requests for capabilities Lectern did not declare |
| `hold.jsonl` | a turn that never finishes |
| `stubborn.jsonl` | ignores SIGTERM and leaves a child process in its group |
| `loud.jsonl` | puts canary strings in every place the logs must not show |
