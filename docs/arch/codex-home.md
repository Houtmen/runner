# Codex home isolation

Set an absolute `CODEX_HOME` in the environment of Runner and its daemon before
starting them to use a separate Codex configuration. Launch Codex with that same
home and sign in using `codex login` when authentication is needed. Do not copy
credential files from another Codex home.

Runner's Codex defaults, model discovery/cache identity, skills and MCP settings
use this configured home. Sessions inherit it; their launch definitions record
an explicit inherited home so resume does not silently switch to a new home if
Runner's environment changes. With no configured home, the existing `~/.codex`
behavior remains the default.

A role can override its session's home using `runner role update <role> --env
CODEX_HOME=<absolute-path>`. Fresh launch, native fork, resume, rollout discovery,
project trust and the Runner hook-server entry use the effective spawn home.
A relative role override resolves against that session's working directory.
Global Settings catalogs still describe the daemon's home rather than one
particular role's override. Use a daemon-wide home when Settings must manage the
same configuration as all Codex roles.

Runner creates a disabled `runner_hooks` MCP entry and project trust in the
selected home's `config.toml`, preserving existing trust choices and unrelated
configuration. It does not copy authentication or configuration to that home.
The home is recorded as a path, not a credential. Changing a role's home affects
new sessions; an existing session resumes from its saved launch definition.
