# Testing & Types — Hardening Roadmap

> Living document. Update as tasks complete or new regression classes surface.

This is the punch list for finishing the FE-hardening work that started after a string of regressions (ask_user panel race, silent skill drop, scheduled-run cancellation, …) made clear that the JS↔Rust boundary needed contract guarantees and that critical state-management code needed test coverage.

## Where we are

Three commits landed:

| SHA       | Step                                | What it actually catches                                                                                                                                                |
| --------- | ----------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `15cf13a` | vitest + 16 seed tests              | `loadSessionData` preservation of FE-only state (the ask_user race); `clearAskUserPending` stale-id guard; event-envelope wire-format keys (`payload.pending_id`, etc.) |
| `ebc1c78` | ts-rs codegen for the event surface | Compile-time error when any Rust type in the `AssistantUiEvent` closure is renamed or has a field added/removed                                                         |
| `ce7e66b` | TS bootstrap on 4 critical files    | Exhaustiveness on the event-reducer switch; typed store interfaces; typed `assistantClient.*` and workspace client invokes                                              |

Scripts now wired:

- `npm test` / `npm run test:watch` — vitest
- `npm run typecheck` — `tsc --noEmit`
- `npm run gen:bindings` — regenerate `src/generated/bindings.ts`
- `npm run lint` — eslint over .js/.jsx/.ts/.tsx
- GitHub CI runs lint + typecheck + tests + `gen:bindings` and fails if `src/generated/bindings.ts` drifts.

## What "completely done" means

We can call this work finished when **all** of the following are true:

1. Every BE→FE plumbing surface (events + Tauri command request/response shapes that the FE actually reads) has generated bindings. Rust renames break the FE build.
2. Every file in `src/` is `.ts`/`.tsx`. `allowJs` is removed; strict mode is on.
3. Every regression we've hit has a test that would have caught it. New regressions land with a failing test before the fix.
4. CI runs `typecheck` + `test` + `gen:bindings` (and fails if bindings drift). Every push is gated.
5. The 4 highest-traffic UI surfaces (Workspace page, Fleet page, AskUserPanel, ChatMessageList) have component-level tests covering at least their happy path.

We're roughly **50-60%** of the way there as of 2026-05-26. P0 closed cleanly; P1-3 and P1-5 are done; P1-1, P1-2, and P1-4 are partially landed (provider-connection bindings + typed client; AskUserPanel/InlineApprovalCard/InlinePathGrantCard converted to `.tsx`; AskUserPanel covered by component tests). Remaining: MCP/skill/path-grant bindings, the ChatMessageList + WorkspaceFilePreviewPanel + useAssistantSession conversions, and ChatMessageList/InlineApprovalCard tests. All of P2 is still ahead.

## House rules in effect today

These already apply — don't wait for the roadmap to finish:

- **Regenerate bindings when Rust shapes change.** `npm run gen:bindings` and commit the diff. Anything that derives `TS` is part of the FE-visible contract.
- **Bindings drift is CI-gated.** CI runs `npm run gen:bindings` and `git diff --exit-code src/generated/bindings.ts`; if Rust `TS` shapes changed without a committed binding update, the build fails.
- **Failing test first when a regression bites.** Write a vitest case that fails on `main`, then ship the fix. The bug becomes a permanent canary.
- **Prefer `.ts`/`.tsx` for new files** in `src/`. Vite consumes either; mechanical conversion of existing `.jsx` can drift in as files are touched.
- **Never delete a failing test to ship.** If it fails, either the test is wrong (fix it) or the code is wrong (fix that). Suppressing is a smell.

## Remaining tasks

### P0 — Completed 2026-05-26

- [x] WorkspaceSnapshot/list/session/file bindings generated from Rust and consumed by `src/workspace/client.ts`.
- [x] CI binding-drift guard added via `npm run gen:bindings` + `git diff --exit-code src/generated/bindings.ts`.
- [x] `src-tauri/tests/prompt_build.rs` covers selected bundled skill content and the no-skill raw-description path through `workspace_agent_runtime_description(...)`.
- [x] `src/pages/Workspace.jsx` converted to `src/pages/Workspace.tsx` with typed snapshot/state/props around the Workspace page shell.

### P1 — Partially completed 2026-05-26

**P1-1. More Tauri command bindings.** _Partial._

- [x] Provider-connection request/response structs (`commands/provider_connections.rs`) + the assistant types they reference (`AuthMode`, `ProtocolFamily`, `ProviderConnection`, `ProviderDescriptor`, `ModelInfo`). `src/assistant/client.ts` provider commands are now typed end-to-end.
- [ ] MCP server commands.
- [ ] Skill commands.
- [ ] Path-grant + permission commands.

**P1-2. Convert the highest-traffic remaining components.** _Partial._

- [x] `AskUserPanel.jsx` → `.tsx`.
- [x] `InlineApprovalCard.jsx` → `.tsx`.
- [x] `InlinePathGrantCard.jsx` → `.tsx`.
- [ ] `ChatMessageList.jsx` → `.tsx` (~700 lines, biggest remaining file).
- [ ] `WorkspaceFilePreviewPanel.jsx` → `.tsx`.
- [ ] `useAssistantSession.js` → `.ts`.

**P1-3. Convert `workspaceStore.js` to `.ts`.** _Done._

- [x] Tile-tree discriminated union + tab/command/store interfaces typed; `Persist edWorkspaceState` pins the Tauri boundary.

**P1-4. Component-level tests for the chat surface.** _Partial._

- [x] `src/test/mockTauri.ts` shared pattern.
- [x] `AskUserPanel.test.tsx` (8 tests: render, options, submit, error-keeps-panel-mounted, snapshot-poll race guard).
- [ ] `ChatMessageList.test.tsx`.
- [ ] `InlineApprovalCard.test.tsx`.

**P1-5. BE integration test for cancel-token registration.** _Done._

- [x] Unit tests in `assistant/runtime.rs` (register+cancel signal, unknown returns false, unregister removes, double-register replaces).
- [x] Integration tests in `src-tauri/tests/cancel_run.rs` pin the convention that all three spawn sites (chat, scheduler, workspace-task) register under DB `run.id`.

### P2 — Longer tail (multi-day, opportunistic)

**P2-1. Convert the remaining `.jsx` files.** _Effort: ~1-2 days, depending on file size._
Touch as you go, don't batch. Order roughly:

- `src/pages/Fleet.jsx` (large, regression-prone).
- `src/pages/Workspace.module.css`-adjacent components.
- `src/components/Settings/*` (many files, each smallish).
- `src/components/AssistantChat/*`.
- `src/components/Canvas/*` (xyflow nodes — careful around d3 typing).
- `src/contexts/*` (small).
- `src/hooks/*`.
- `src/utils/*`.
- `src/stores/chatManagerStore.js`.
- The remaining test files — rename `.test.js` → `.test.ts` and tighten the assertions where the new types help.

**P2-2. Drop `allowJs`; tighten compiler.** _Effort: ~30min once P2-1 is done._

- Set `"allowJs": false` and `"checkJs": true` (defensive — no .js should remain, but catches accidents).
- Enable `noUncheckedIndexedAccess`, `noImplicitOverride`, `exactOptionalPropertyTypes`. Each will surface real bugs; fix them.

**P2-3. Coverage tracking.** _Effort: ~1-2h._

- `vitest --coverage` with c8.
- Aim for 80% on `src/assistant/` first; ratchet up across the codebase.
- CI gate: fail the build if coverage drops more than 2% in any package.

**P2-4. Convert FE test files to `.ts`.** _Effort: 30min._
After enough of P2-1 lands. Mechanical.

**P2-5. Provider adapter tests.** _Effort: 1 day._
The Anthropic / OpenAI / Claude Code stream parsers in `src-tauri/src/assistant/providers/` have zero unit coverage. A regression here would be catastrophic. Each adapter:

- Add `tests/<adapter>_stream.rs`.
- Feed a recorded stream (capture a few real ones into `tests/fixtures/`).
- Assert the parsed events match a known sequence.

**P2-6. End-to-end smoke tests.** _Effort: 1-2 days, plus ongoing maintenance cost._
Defer unless P0-P2 stops catching regressions. Tauri-driver + Playwright on the dev build. Cover just the golden path: open workspace → send message → see streaming → see ask_user panel → submit answer.

## Out of scope (explicitly deferred)

- **Visual regression / screenshot diffing.** None of our regressions have been visual. Setup cost (Percy / Chromatic) outweighs payoff at our current size.
- **Full `tauri-specta` instead of `ts-rs`.** ts-rs gives us what we need (typed shapes). tauri-specta's additional value (typed `invoke` wrappers) is nice but not urgent; revisit if invoke-shape drift becomes a recurring bug.
- **Storybook.** Component-level vitest tests cover the same need with less infrastructure.
- **Performance / bundle-size regression gates.** Not the class of bug we've been hitting.

## When in doubt

If you're picking up this roadmap mid-stream:

1. Check the top of the file for any tasks marked `[in progress]` or `[blocked]`.
2. Default to P0 → P1 → P2 ordering. Skip a P0 only if blocked.
3. One task per commit; each task should leave the tree green (typecheck + test + build).
4. Update this file in the same commit that completes a task — strike through the bullet or remove it.
