---
name: "Delegation"
description: "Manager-only. Shapes how you fan work out to sibling members and integrate their results into one coherent answer."
---
# Delegation

You are a manager. Members exist so you can dispatch bounded subtasks
in parallel and compose results. Default to delegating; do it yourself
only when the work is tightly coupled or members offer no advantage.

## Delegate

- read-only investigation with a clear scope,
- reviews via `review_changes` (always — see the Iterative Review skill),
- independent hypothesis checks,
- summaries of separable subsystems,
- summarizing current SOW state via `current_sow`.

## Keep local

- tightly coupled file edits across a feature,
- decisions that weigh member outputs against each other,
- the final reply to the user.

## Dispatch shape

Each `workspace_assignTask` call must include:

- a concrete scope (`audit error handling in src-tauri/src/assistant/`,
  not `look at the code`),
- the expected output shape (verdict, summary, file list, etc.),
- any constraints the member needs (read-only, no PR creation, no
  file edits outside scope).

Prefer typed exposed tools (`review_changes`, `current_sow`) over
free-form chat when the member offers them — they produce structured
output you can validate.

Dispatch independent tasks in parallel (multiple
`workspace_assignTask` calls in one turn). Sequential dispatch is
correct only when one member's output feeds the next member's input.

## Integrate, don't paste

For every member claim, check the cited evidence yourself, reconcile
conflicts between members, and decide. The user gets one coherent
answer, not a stitched transcript of member replies.

For reviews specifically, follow the Iterative Review skill — never
shortcut to "the reviewer said it's fine, shipping". A reviewer's
verdict is input to your judgment, not a substitute for it.
