---
name: "SOW Workflow"
description: "Manager-only. Tracks all project work as durable Statements of Work in .clai/sow/. Attach when work needs auditable planning across runs."
---
# SOW Workflow

> **Binding skill.** While attached, the procedure here is non-optional.
> The user controls it by attaching or removing the skill, not by asking
> you to skip it mid-run.

## Trigger

This procedure runs on every run that will modify project files —
whether autonomous (scheduled or manual automation) or user-driven
(the user asks you to implement, fix, or change something).

It does NOT run for:

- pure-conversation runs (explanation, design discussion),
- read-only investigation with no file edits,
- memory-only updates inside `.clai/memory/`.

If you are unsure whether a user request will produce edits, ask once.
Do not open a SOW for clarification rounds.

## On every triggering run

1. Read `.clai/sow/index.md` (create if missing). Note which SOWs are
   `open`, `in-progress`, or `completed`.
2. If a SOW is `in-progress`, resume it. Only one SOW may be
   in-progress at a time.
3. If none is in-progress, pick the highest-priority `open` SOW and
   transition it to `in-progress` (timestamp the transition in its
   `state.md`).
4. Modify project files only in service of the current SOW.
5. If you discover work that doesn't fit the current SOW, do NOT
   silently expand scope. Draft a new SOW in `open`, then return to
   the current one.
6. End of run: update the SOW's `state.md` — what was done this run,
   what remains, any blockers.

## States

- `open` — defined but not started,
- `in-progress` — active (only one at a time),
- `completed` — implementation done AND validation recorded,
- `closed` — archived or superseded.

## Layout

```
.clai/sow/
├── index.md
└── <sow-id>/
    ├── scope.md       # what and why
    ├── plan.md        # how
    ├── state.md       # current status, updated every run
    ├── decisions.md   # noteworthy choices
    └── validation.md  # evidence of completion
```

## Completion gate

Mark a SOW `completed` only when:

- `validation.md` lists evidence (commands run with outputs, tests
  passed, manual checks done),
- if the Iterative Review skill is also attached to you, every
  reviewer returned `production_quality` for the change.

## Refusal

If the user asks you to skip the SOW system "just this once", refuse
and quote this skill. To turn off SOW tracking, remove this skill
from your agent configuration.

## Blocked states

If `.clai/sow/` is outside your `execution.filesystem.extra_paths`,
report BLOCKED with the required path. Do NOT silently skip SOW
updates — that produces a misleading "everything fine" run with no
audit trail.

## Priority over memory

Memory is "what I learned". SOWs are "what work exists". Do not put
work plans in `.clai/memory/state.md`; do not put learned heuristics
in SOW files. Memory updates happen at the very end of a run, after
the SOW `state.md` is updated.
