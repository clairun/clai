# CLAI demo video script

A ready-to-shoot script for CLAI's promo video. Two cuts: a short silent loop
for the website hero (autoplay) and a longer narrated walkthrough for the
README / YouTube / Show HN. Both follow one scenario that hits every
differentiator: **provider choice → MCP tools → local sandbox → delegation to
a specialist → inspectable transcript → memory/artifacts → scheduling → Fleet
supervision.**

## The scenario

> You just cloned an open-source repo and want a second pair of eyes before you
> ship. With CLAI you spin up a workspace, point it at the code, and let a small
> agent team review it — using your real files and shell, under permissions you
> set, while you watch every step. Then you make it run every morning.

It's relatable to a dev audience, needs no fake data, and the
"delegate + watch the live transcript" moment is the wow.

## Pre-recording checklist

- **Clean profile:** curated workspace list, no personal repo names, no secrets
  in provider settings, scrub API keys from any visible field.
- **Window size:** record at 1920×1080 (or 2× retina downscaled) — crisp text
  matters most.
- **Theme:** pick one and stick to it. The website is dark, so **dark theme**
  keeps the clip cohesive when embedded.
- **Cursor:** move slowly and deliberately; consider a cursor-highlight/click
  tool.
- **Typing:** pre-write the messages (below) so you don't fumble; paste or type
  at a calm pace.
- **A repo to point at:** clone something small and recognizable into the
  workspace dir beforehand (CLAI itself works — meta but fine).
- **Trim the waiting:** model "thinking" time is dead air — speed-ramp (4–8×)
  over any spinner.

## Cut 1 — Website hero loop (silent, captioned, ~28s, loops)

Replaces the static `screenshot.png`. No audio; large on-screen captions; ends
so it loops cleanly.

| Time | On screen | Caption (big, bottom-center) |
|------|-----------|------------------------------|
| 0:00–0:05 | Fleet view, click **New workspace** | **Build a team of AI agents — on your desktop** |
| 0:05–0:12 | Workspace settings: pick provider, attach an MCP server, set shell to **Restricted** | **Pick a provider. Attach MCP tools. Scope what it can run.** |
| 0:12–0:19 | Type request; main agent fires tool-call chips (ls/read) | **Real tools — your files & shell, under your rules** |
| 0:19–0:25 | Add Code Reviewer; delegation task card + live transcript sliding in | **Delegate to specialists. Watch every step.** |
| 0:25–0:28 | Toggle **Schedule**, cut to Fleet view card floating up | **Run on a schedule. Supervise the fleet.** |
| (loop) | Hard cut back to Fleet view | — |

Keep each caption ≤6 words, on screen ~3s. No end card (it loops).

## Cut 2 — Full walkthrough (~90s, narrated)

Timecodes, action, exact text to type, and a voiceover (VO) line per scene.

### Scene 1 — Hook (0:00–0:08)
- **Action:** Open on Fleet view, click **New workspace**, name it `repo-review`.
- **VO:** "CLAI is a desktop app for building and supervising small teams of AI
  agents. Let's put one to work."

### Scene 2 — Configure (0:08–0:22)
- **Action:** Open workspace settings (gear). Choose a provider (show the
  dropdown: Claude Code / OpenAI / Anthropic). Attach an MCP server. Set shell
  mode to **Restricted**.
- **VO:** "Each agent picks its own provider. Attach MCP tools once, and scope
  exactly what it's allowed to run on your machine."

### Scene 3 — Main agent uses real tools (0:22–0:40)
- **Type:**
  > I just cloned a repo into this workspace. Take a look around, tell me what
  > it does, and flag anything risky before I ship.
- **Action:** Main agent issues tool calls — show the `ls` / `read_file` chips
  expanding. Speed-ramp any wait.
- **VO:** "It doesn't just chat — it uses your real filesystem and shell, gated
  by the permissions you set."

### Scene 4 — Delegation (the wow) (0:40–1:02)
- **Type:**
  > Have the Code Reviewer go through the auth code in detail and write the
  > findings to a file I can read.
- **Action:** Open Agents drawer → add **Code Reviewer** template. Main agent
  delegates → a task card appears → click it → the live transcript panel slides
  out showing the helper's conversation, tool calls, and verdict.
- **VO:** "Add a specialist from a template, and the main agent delegates to
  it — and you can watch the entire transcript. Nothing happens in a black box."

### Scene 5 — Persisted output (1:02–1:14)
- **Action:** Verdict returns; main agent summarizes inline. Open the drawer →
  Artifacts → preview the generated `review.md` (rendered markdown).
- **VO:** "Findings persist as memories and artifacts, right in the workspace —
  readable any time."

### Scene 6 — Schedule + supervise (1:14–1:26)
- **Type:**
  > Looks good. Run this review every morning so I catch regressions early.
- **Action:** Toggle **Schedule** → set interval. Cut to Fleet view: the
  scheduled workspace floats to the top; point out the attention flags.
- **VO:** "Make it periodic, and the Fleet view supervises everything — what's
  scheduled, what needs you, with a live preview."

### Scene 7 — End card (1:26–1:32)
- **Action:** CLAI logo + tagline + URL.
- **On-screen text:** CLAI — build, run & supervise AI agent teams ·
  *juacker.github.io/clai* · macOS · Windows · Linux · MIT
- **VO:** "CLAI. Open source, local-first, provider-agnostic. Link in the
  description."

## Copy-paste conversation

```
1. I just cloned a repo into this workspace. Take a look around, tell me what
   it does, and flag anything risky before I ship.

2. Have the Code Reviewer go through the auth code in detail and write the
   findings to a file I can read.

3. Looks good. Run this review every morning so I catch regressions early.
```

> Do a dry run first so the agent's real responses are sensible, then record the
> clean take. If a real run is too slow/unpredictable for a polished take, record
> the UI beats and the actual tool-call/transcript visuals separately and cut
> them together — the *interface* is the star, not the model's prose.

## Production / export notes

- **Format:** export **MP4 (H.264) + WebM** for the site, not GIF — a 30s GIF is
  huge and grainy; a muted autoplay-loop `<video>` is smaller and sharper. Keep
  a poster frame (the current screenshot) for first paint.
- **Length discipline:** hero loop ≤30s, walkthrough ≤95s. Cut ruthlessly.
- **Emphasis:** subtle zoom-ins (1.1×) on tool-call chips and the transcript
  panel — those are the proof points.
- **Captions:** burn in captions on the hero loop (it autoplays muted); add real
  subtitles to the walkthrough.
- **Music:** light, low ambient bed for the walkthrough; none for the loop.
