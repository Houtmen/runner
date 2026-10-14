# Global session limit

Set `RUNNER_MAX_SESSIONS` to a positive integer in Runner's daemon environment
before starting it. An unset value preserves unlimited admission. Invalid or
zero values fail daemon startup with an explicit error.

One daemon-wide atomic gate covers all Runner-owned PTYs: direct chats, mission
slots, native forks, restarts, resumes, shells and CLI updater terminals. Headless
fork materialization also reserves capacity while its temporary process runs.
When full, new starts fail with `global session limit reached`; there is no
hidden queue or automatic retry. Stop a session and retry explicitly.

Capacity is reserved before runtime spawn and returned on failed spawn,
confirmed exit, or successful termination. Stop failures and unknown liveness
retain capacity, avoiding accidental over-admission. Existing cancellation and
first-turn timeout paths terminate their runtime and release through that same
boundary. Admission after daemon restart counts newly recreated processes,
not stopped database rows. The setting does not kill existing sessions.

A CLI's own internal subagents are not separate Runner sessions and are not
counted independently. The limit is per daemon, not a machine-wide OS quota.
