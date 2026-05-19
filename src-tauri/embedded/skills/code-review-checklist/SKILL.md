---
name: "Code Review Checklist"
description: "Recommended for reviewer member agents. Review code across correctness, security, maintainability, concurrency, performance, and test quality with file:line evidence."
---
# Code Review Checklist

Use this checklist when acting as a code reviewer. Be read-only unless the caller explicitly asks for implementation separately.

Every finding must include evidence: a `file:line` reference, a specific behavior trace, or command output. Avoid style-only feedback unless it affects maintenance, reliability, or user-facing behavior.

## Accessing the change

Read the change from the most authoritative source available:

- Local refs first — `git diff`, `git show`, `git log` against the supplied range whenever the working tree contains the change.
- Local files — open the paths directly.
- Hosted forge URLs — fetch only when the change is not reachable locally.

A public `web.fetch` against a GitHub URL returns 404 or a login wall on private repositories. When the fetch fails (or you can predict it will, e.g. the URL targets a private org), fall back to the GitHub CLI if shell access is available:

- `gh pr view <url-or-number> --json title,body,baseRefName,headRefName,files,additions,deletions`
- `gh pr diff <url-or-number>` — full unified diff.
- `gh api repos/{owner}/{repo}/pulls/{number}/files` — file-level metadata.
- `gh auth status` — verify auth before the calls above when in doubt.

If neither web fetch nor `gh` can reach the change, do not guess. Return a single finding stating the scope was inaccessible and ask the caller to paste the diff or grant the needed access.

## Dimensions

Check these dimensions on every review:

- Functional correctness: changed behavior, edge cases, state transitions, validation, data loss, migrations, and compatibility.
- Security: injection, path traversal, authentication, authorization, secret exposure, unsafe deserialization, SSRF, and command execution.
- Code smells: bloaters, object-orientation abusers, change preventers, dispensables, couplers, and obfuscators.
- Error-prone patterns: null or option misuse, type coercion, arithmetic hazards, incomplete cleanup, unhandled errors, and fragile tests.
- Concurrency: shared mutable state, races, deadlocks, cancellation, ordering, and async blocking.
- Performance: algorithmic regressions, N+1 work, unbounded loops, memory growth, missing timeouts, and excessive I/O.
- Maintainability: clear ownership, local consistency, unnecessary abstractions, unclear names, and duplicated logic.
- Tests: missing coverage for risky behavior, tests that assert implementation details, brittle sleeps, and untested error paths.

## Output

Return a structured verdict:

```json
{
  "verdict": "production_quality",
  "findings": []
}
```

Use `needs_work` when there is any blocker or major issue. Findings should include `dimension`, `severity`, `description`, and `evidence`.
