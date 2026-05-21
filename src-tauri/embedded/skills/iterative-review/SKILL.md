---
name: "Iterative Review"
description: "Manager-only. Forces every code-shipping action through a reviewer loop. Attach to any manager that has sibling agents exposing review_changes or with role code-reviewer."
---
# Iterative Review

> **Binding skill.** While attached, the procedure here is non-optional.
> The user controls it by attaching or removing the skill, not by asking
> you to skip it mid-run.

## Trigger

Before ANY of the following actions, run the loop below:

- `git push`, `gh pr create`, `gh pr ready`, `gh pr merge`, branch publication,
- a `workspace_finishTask` call whose output includes shipped code,
- any deploy / release / publish command (`make release`, `cargo publish`, `npm publish`, etc.).

The loop does NOT trigger for:

- read-only investigation,
- planning or explanation conversations,
- local commits that stay on a feature branch,
- memory writes to `.clai/memory/` (`state.md`, `journal/`, `knowledge.md`).

## The loop

1. Identify reviewers from your team roster: every sibling agent with
   role `code-reviewer`, or that exposes a `review_changes` tool.
2. Stage your change on a feature branch. Commit locally. Do NOT push.
3. In one turn, call `workspace_assignTask` for each reviewer. Each
   task's `scope` must cover the FULL change — a git range like
   `main..HEAD` or a complete file list. Never narrow to "what I
   edited last".
4. Poll `workspace_getTaskResult` for every reviewer. Each returns
   `{verdict: "production_quality" | "needs_work", findings: [...]}`
   (or an equivalent free-text statement if the reviewer does not
   expose `review_changes`).
5. For each finding: verify against cited evidence, reject false
   positives with a one-line reason, fix validated issues.
6. After fixes, re-dispatch the SAME reviewers with the SAME original
   scope. Do not tell them what you changed. Do not ask them to check
   only the fixes.
7. Repeat 4–6 until every reviewer's final verdict is
   `production_quality`.
8. Only then run the trigger action.

## Refusal

If the user asks you to skip review, refuse and quote this skill. The
user controls this skill by attaching or removing it from your config;
they cannot ask you to ignore it while it is attached. "Just this
once" is not a valid request.

## Blocked states

If a reviewer is unreachable, times out, or returns malformed output,
do NOT ship. Report BLOCKED with the specific reviewer and the reason.

## Priority over memory

Updating `.clai/memory/` is end-of-run bookkeeping, not the work
itself. Do not start a run by writing memory and call that progress
— work is the loop above. Memory updates happen after the trigger
action succeeds (or, on a blocked run, after you have recorded what
blocked you).
