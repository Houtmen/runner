# Windows orchestration acceptance

CI builds and tests the selected commit on Windows x64 and macOS. The deterministic
planner/builder/reviewer regression uses a mock runtime: three roles share the
global gate, their configured Codex homes reach their spawn definitions, a
repair ID is idempotent, a stopped builder explicitly changes model and resumes
with its original native key, and review completion remains terminal on replay.
Existing built-CLI/isolated-daemon and Windows batch/PTY tests cover the transport
and command boundary. Mock results do not establish subscription authentication.

After Windows tests, native resource verification and lockfile checks pass, the
workflow packages `Runner.exe`, `runner-agent-cli.exe`, pinned Microsoft-signed
ConPTY files, license notices, commit identity and SHA-256 sums. The artifact
`runner-windows-x64-<commit>` is a development acceptance build, not a signed
installer or official Runner release. Download it from the matching CI run.
No user data, credentials, logs or native transcripts are packaged.

## Separately authorized native checks

Use a Windows x64 development host with a separate Runner development database
and an absolute CODEX_HOME. The user signs in to that home themselves; do not copy
Codex/OmO configuration or credentials. Real subscribed-agent calls require
explicit authorization and a bounded test goal. No paid API fallback is allowed.

For each of Codex, Claude Code and Antigravity independently:

1. Verify executable/version and existing subscription sign-in.
2. Start one bounded chat, verify the selected model, cancel/stop it, and resume
   its original native conversation. Verify history rather than only its ID.
3. Verify timeout/cancel cleanup and capacity release with RUNNER_MAX_SESSIONS.
4. For Codex, verify isolated trust/hooks/skills/model discovery do not write the
   standard home. Choose a model inside the CLI, then synchronize an owned
   rollout and verify model plus history after closing/reopening Runner.
5. For Claude/Antigravity, verify their native model persistence separately;
   transcript sync is currently refused because the format is unverified.
6. Run one planner/builder/reviewer mission with an explicit review budget,
   require a repair, then verify completion/exhaustion and restart persistence.

Record candidate commit, Windows version, commands and their exit statuses,
actual behavior, and sanitized logs. Keep failed, skipped and unrun checks
separate. Do not call the native workflow ready until those checks pass.
For a provider change, create a new chat using a reviewed handoff file and retain
its source session as the original history; do not claim native session transfer.
