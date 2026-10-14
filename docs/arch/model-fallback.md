# Model preferences and manual fallback

Configure an ordered list on a reusable role:

```sh
runner role models coder --prefer gpt-6-sol --prefer gpt-6-luna
runner role models coder
```

The first preference becomes the role's existing single `model` field, so older
clients and launch paths keep working. Preferences are stored separately by
role and runtime; changing the runtime does not import its old model list.
`--clear` removes the list and retains the existing single model as the default.
Blank names are rejected; duplicates are removed while retaining order.

When a CLI reports a model or quota problem, inspect the configured alternatives
and choose explicitly:

```sh
runner session models <session>
runner session stop <session>
runner session models <session> --choose gpt-6-luna
runner session resume <session>
```

Choosing requires a stopped, unarchived, role-backed session and a model from
its configured preferences. It never starts an agent or retries automatically.
The recorded launch definition changes only its model; native conversation ID,
arguments, environment and working directory are retained for normal resume.
If the runtime cannot resume that native conversation, Runner must report the
failure; a new conversation is not claimed to retain its history.

There is no automatic paid API route or provider/runtime change. These controls
are available in the CLI and socket API; no new desktop controls are added.
The CLI's own terminal displays quota/model failures. Runner does not guess a
failure category by scraping arbitrary terminal text. Runtime-only sessions
keep the existing single-model behavior and need a role to configure this list.
