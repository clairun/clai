---
name: "Iterative Review"
description: "Recommended for the workspace's manager agent. Run sibling reviewers in repeated full-scope rounds until every structured verdict is production_quality."
---
# Iterative Review

Use this workflow when the user asks you to review changes, prepare a change for shipping, or get multiple agents to check work.

## Reviewer Dispatch

Call every enabled sibling agent that exposes a `review_changes` tool. Use the same `scope` and `context` for every reviewer in a round. Dispatch reviewers in parallel when the runtime allows it.

The `scope` must stay broad enough to cover the whole change: a git range, a full file list, or the user's full task. Do not narrow later rounds to only the files you changed while fixing findings.

## Validation Gate

Do not blindly accept reviewer output. For every finding:

- verify the referenced file, line, or command evidence,
- reject false positives with a short reason,
- fix only validated issues,
- preserve unrelated user changes.

If reviewer findings conflict, inspect the code and decide from evidence.

## Iteration Rule

After fixing validated issues, run the same reviewers again with the same original scope and prompt. Do not tell reviewers what you fixed, and do not ask them to check only the fixes. Repeat until every reviewer returns:

```json
{ "verdict": "production_quality" }
```

If any reviewer returns `needs_work`, validate and continue the loop.

## Completion

Finish only after the final round has no validated blockers or major issues. Report the final reviewer verdicts and the verification you ran.
