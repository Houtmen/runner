# Bounded review cycles

Before requesting repairs, configure a mission's budget as the person:

```sh
runner signal review_policy --mission <mission> --payload '{"max_repairs":3}'
```

The reviewer requests each repair through a typed signal rather than a free-text
repair instruction. A stable request ID makes a retried invocation idempotent:

```sh
runner signal repair_requested --payload '{"request_id":"review-1","target":"coder","text":"Fix the regression in …"}'
runner signal review_complete --payload '{"text":"No remaining findings"}'
```

Outside a mission session, pass `--mission <mission> --as <reviewer-handle>`.
Runner routes accepted repairs to `target` (default: lead). The mission log is
both the persistent counter and policy: all writers check and increment under
the same exclusive event-log lock. Resume and daemon restart replay the same
budget and counter; neither resets them. A configured budget cannot be changed
for that mission. Start another mission for a new review budget.

At the first over-budget attempt, Runner records `review_exhausted` and returns
a nonzero CLI result. Further repairs are refused. A clean review records
`review_complete`; repairs are refused after that terminal result too. These
are review end states in the mission's event feed, separate from the mission's
existing lifecycle status. A zero budget allows no repair requests.

Crews without a policy retain unlimited admission. Existing stored crew prompts
are not silently overwritten: update their conventions to use these typed
repair requests when enabling a budget. Free-text messages and work done inside
a CLI are not repair requests and cannot be counted reliably. This is protocol
admission, not text inference or a provider-side token limit. On rejection,
stop the repair loop and ask the person; do not bypass it with a normal message.
