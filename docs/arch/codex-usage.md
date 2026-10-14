# Codex usage probe

Runner starts the configured Codex executable as a read-only app-server helper,
using the daemon's environment and Codex home. It initializes the connection,
reads account/read without requesting a proactive token refresh, then reads
account/rateLimits/read. A missing account produces the Codex sign-in message;
launch/process tracking, initialization, account, limits and timeout failures
have distinct messages. Account details/error text/tokens are never logged;
only numeric protocol/OS codes are recorded for diagnosis.

For Codex 0.162.1, the background-poll parameter excludeResetCreditDetails=true
skips the separate reset-credit detail lookup, which is unused by Runner's usage
bars. The parameter is defined in OpenAI's tagged app-server protocol:
https://github.com/openai/codex/blob/rust-v0.162.1/codex-rs/app-server-protocol/src/protocol/v2/account.rs
Older CLIs returning invalid params (-32602) get one compatibility read with
empty params and a different request ID. The same deadline covers the entire
handshake/account/limits sequence and the retry. Authentication/provider errors
are not retried, and supportsLunaReserve is not enabled. No model/session or
paid API fallback is started by the probe.

The prior result is cached; click the Usage refresh button after signing in.
An account used by a role-specific home is independent of the daemon-wide home
used by Usage and Settings catalogs. Settings → Agent controls shows those
scopes. Working chats alone do not prove the global usage probe is authenticated
or that the server can return limits.

A cross-platform mock app-server test covers successful limits, signed-out
accounts, each failure phase and bounded legacy compatibility. Windows uses a
batch shim backed by Console.In.ReadLine: repeated cmd set /p loses buffered
pipe lines and is not a valid simulation of Codex's persistent RPC reader.
Real subscription limits still require verification on the user's computer.
