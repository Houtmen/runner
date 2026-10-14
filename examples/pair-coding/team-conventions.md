This crew works as a two-person coding loop. Runner messages are the coordination and review record. The review loop always runs on the task-specific feature branch's working-tree diff before any commit, push, or pull request.

`coder` owns implementation, tests, and follow-up fixes. Before editing, `coder` checks the repository status and current branch, then creates and switches to a task-specific feature branch unless the mission names one. `coder` never implements directly on `main`, `master`, or another long-lived shared branch. If the working tree already has unrelated changes or branch creation would be unsafe, `coder` stops and asks instead of stashing, resetting, or overwriting anything. After the implementation and relevant checks pass, `coder` messages `reviewer` with the branch name, summary, changed files, and checks run. `reviewer` stays idle until that handoff, critiques the working-tree diff without editing it, and replies with must-fix findings first or a clean-review statement. The loop repeats until review is clean: a task is done when it is implemented, the relevant checks pass, and `reviewer` reports no remaining must-fix issues.

The human can write into the crew channel. When a message from `human` arrives, read it with `runner msg read` and reply to the channel with `runner msg post "<text>"` (no `--to` flag). All changes stay in the working tree unless the mission goal explicitly authorizes a specific git side effect. Never merge unless the human explicitly asks.

For repair requests, `reviewer` uses `runner signal repair_requested --payload
'{"request_id":"<unique-review-id>","target":"coder","text":"<findings>"}'`.
Reuse the ID only when retrying the exact same request. A human may configure a
mission's repair budget using `review_policy`; do not change that policy. If a
repair request is refused, stop the loop and ask the human instead of sending
another repair instruction by message. When review is clean, emit
`runner signal review_complete --payload '{"text":"No remaining findings"}'`
and send the normal clean-review summary. A later, separate task needs a new
mission after that review terminal state.
