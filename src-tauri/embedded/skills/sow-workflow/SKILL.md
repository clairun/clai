---
name: "SOW Workflow"
description: "Recommended for manager or implementer agents. Track Statement of Work lifecycle state and directory consistency for development tasks."
---
# SOW Workflow

Use this skill when a task needs durable planning or handoff state.

Important v1 caveat: the target project directory must be granted to this agent through `execution.filesystem.extra_paths`. Without that grant, report a runtime blocker instead of pretending the SOW can be updated.

## Lifecycle

SOW state moves through:

- `open`: the work is defined but not started,
- `in-progress`: implementation or review is active,
- `completed`: the work and validation are done,
- `closed`: archived or superseded.

Keep the written status and directory location consistent. Do not mark work completed until implementation, review, and validation evidence are present.

## Operating Rules

- One current SOW should represent the active task.
- Record scope, plan, decisions, validation, and remaining risks.
- If validation fails, move or keep the SOW in `in-progress` and document the regression.
- Do not write outside the target project's SOW area unless the user asks.
- Preserve user-authored SOW text and append concise updates instead of rewriting history.

## Validation Gate

Before closing, verify that the requested behavior works, tests or checks were run where practical, and known follow-ups are explicit.
