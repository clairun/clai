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

We're roughly **70-75%** of the way there as of 2026-05-26. **P0 and P1 complete; P1-1's skill-binding carve-out closed under P2-1b.** P2 is underway: the three pinned leaf components + a batch of trivial leaves/indexes are converted (30 `.ts`/`.tsx` files now). Remaining: ~53 `.jsx` + ~22 non-test `.js` to convert (the bulk of the effort — Fleet, Settings, Canvas, the chat/chart components, contexts, `api/client.js`), then drop `allowJs` (P2-2), coverage (P2-3), provider-adapter tests (P2-5), and E2E (P2-6).

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

### P1 — Completed 2026-05-26

**P1-1. More Tauri command bindings.** _Done (one carve-out)._

- [x] Provider-connection request/response structs + assistant types (`AuthMode`, `ProtocolFamily`, `ProviderConnection`, `ProviderDescriptor`, `ModelInfo`). `src/assistant/client.ts` typed end-to-end.
- [x] MCP server commands (`Create/UpdateMcpServerRequest`, `McpServerAuthRequest/Response`, `McpServerResponse`, `McpServerTransport`, `McpServerIntegrationType`).
- [x] Path-grant + permission commands (`PermissionRequest`, `SegmentDecision`, `SegmentApproval`, `PathGrantRequest`, `PathGrantDecision`, `FilesystemPathAccess`). `permissions/client.ts` + `pathGrantsClient.ts` converted + typed; inline cards now use the generated types.
- [ ] **Skill catalog bindings — deferred.** `SkillSourceConfig` + `SkillDefinition` closure is heavy (`serde(flatten)` over the config struct) and the skills surface is low-regression-risk. Pick up when the skills settings screen needs hardening.

**P1-2. Convert the highest-traffic remaining components.** _Done._

- [x] `AskUserPanel.jsx`, `InlineApprovalCard.jsx`, `InlinePathGrantCard.jsx`, `WorkspaceFilePreviewPanel.jsx` → `.tsx`.
- [x] `ChatMessageList.jsx` → `.tsx` (~700 lines; discriminated-union narrowing on `ContentPart`, full prop interfaces).
- [x] `useAssistantSession.js` → `.ts`.
- Note: `MarkdownMessage`, `StreamingMarkdown`, `VirtualizedList` remain `.jsx`; consumers pin them via typed casts. Converting those three is folded into P2-1.

**P1-3. Convert `workspaceStore.js` to `.ts`.** _Done._

- [x] Tile-tree discriminated union + tab/command/store interfaces typed; `PersistedWorkspaceState` pins the Tauri boundary.

**P1-4. Component-level tests for the chat surface.** _Done._

- [x] `src/test/mockTauri.ts` shared pattern + `src/test/setup.js` `scrollIntoView` stub.
- [x] `AskUserPanel.test.tsx` (8), `ChatMessageList.test.tsx` (5), `InlineApprovalCard.test.tsx` (4). FE suite: 33 tests across 5 files.

**P1-5. BE integration test for cancel-token registration.** _Done._

- [x] Unit tests in `assistant/runtime.rs` + integration tests in `src-tauri/tests/cancel_run.rs` pinning the all-spawn-sites-use-run.id convention.

### P2 — Longer tail (multi-day, opportunistic)

**P2-1. Convert the remaining `.jsx` files.** _In progress — ~53 `.jsx` + ~22 non-test `.js` left as of 2026-05-26 (30 files already `.ts`/`.tsx`)._
Touch as you go, don't batch. Done so far this pass:

- [x] Pinned leaf components: `MarkdownMessage`, `StreamingMarkdown`, `VirtualizedList` (`<T,>` generic, `memo(Inner) as typeof Inner` export). Casts removed from consumers.
- [x] Trivial leaves: `utils/openExternal`, `hooks/useDebounce`, `hooks/usePlatform`, and pure re-export indexes (assistant, commands, Dashboard, TerminalEmulator, TileView, ContextPanel, Settings).

Remaining, order roughly:

- `src/pages/Fleet.jsx` (large, regression-prone).
- `src/components/Settings/*` (many files, each smallish).
- `src/components/AssistantChat/*` (the rest beyond ChatMessageList).
- `src/components/Chat/*` chart blocks + `ToolBlock`, `DesktopChatPanel`.
- `src/components/ContextPanel/*`, `TabBar`, `TabContent`, `TabView`, `TileView`, `TerminalEmulator/*`, `Dashboard`, `Canvas/*`.
- `src/components/Canvas/*` — **gotcha**: `Canvas/index.js` re-exports a nonexistent `TextNode`; drop it when converting. xyflow/d3 typing needs care.
- `src/contexts/*`, remaining `src/hooks/*`, `src/utils/*`, `src/stores/chatManagerStore.js`.
- `src/api/client.js` (486 lines, mixed concern) — also where the skill-command consumer typing lands.
- `src/App.jsx`, `src/main.jsx`, `src/Routes.jsx`, `src/layouts/MainLayout.jsx`, `src/pages/NotFound.jsx`.
- The remaining test files — rename `.test.js` → `.test.ts`.

**P2-1b. Skill-catalog bindings.** _Done 2026-05-26._

- [x] `SkillSourceKind/Config`, `SkillDefinition`, `SkillSourceDiagnostic`, and the `skills.rs` request/response structs derive `TS`. `#[ts(flatten)]` added alongside `#[serde(flatten)]` on `SkillSourceResponse`. Consumer typing (`api/client.js` skill commands) lands when that file is converted under P2-1.

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
