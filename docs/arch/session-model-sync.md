# Native model changes and context

Codex persists its chosen model in a rollout `turn_context.payload.model` record,
with the owning UUID in `session_meta.payload.id`. This was checked against
OpenAI's [TurnContextItem protocol](https://github.com/openai/codex/blob/main/codex-rs/protocol/src/protocol.rs)
and Runner's existing rollout identity decoder. A model chosen inside the native
CLI becomes durable when that CLI records its next turn context.

After using the CLI's own model picker, synchronize the selected model:

```sh
runner session sync-model <session> --transcript <native-rollout.jsonl>
```

The command verifies the transcript belongs to the captured native session,
reads at most 1 MiB of header and 1 MiB of tail, and uses the latest complete
turn-context record. A mismatched identity, missing model, unsupported runtime,
archived session or concurrent resume fails without changing the model.
It updates only Runner's recorded launch model and emits the existing session
update event. It does not start an agent, copy credentials, modify a native
transcript or replace the conversation ID. Normal resume therefore uses the
same native conversation with the synchronized model and saved launch context.
This is explicit synchronization, not continuous scraping of terminal output.

Claude Code and Antigravity model persistence have not been verified by this
change. Their native model controls remain available; no inferred model or
fictional cross-provider history is recorded. Real native model-switch/history
smoke needs an authorized host and subscription authentication.

A provider/runtime switch is a new conversation. Write an explicit handoff file
with the goal, decisions, relevant paths, current diff and checks, then create a
new chat with the receiving runtime:

```sh
runner chat start --runtime claude-code --prompt-file handoff.md --cwd .
```

Keep the old session as the original history; the handoff is a summary, not a
native session import. Review the file before sending it, and do not include
credentials. Starting that receiving agent requires the normal user authorization.
