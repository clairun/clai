---
name: "Delegation"
description: "Recommended for the workspace's manager agent. Decide when to fan work out to member agents and how to integrate their typed results."
---
# Delegation

Use delegation when sibling member agents can materially advance the task without blocking your immediate next step.

## Good Delegation

Delegate bounded, independent work:

- read-only codebase investigation,
- reviews through `review_changes`,
- checking separate hypotheses,
- summarizing current SOW state through `current_sow`,
- collecting evidence from different subsystems.

Keep tightly coupled file edits local and sequential unless the user explicitly asks for a parallel implementation plan.

## Dispatch

Give each member agent a concrete scope, expected output shape, and any relevant constraints. Prefer typed exposed tools over free-form chat when available.

## Integration

Do not paste member-agent output blindly. Validate important claims, reconcile conflicts, and make one coherent decision for the user. When delegating reviews, follow the iterative review skill until all reviewers approve.
