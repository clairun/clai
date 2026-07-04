<p align="center">
  <img src="public/icon.svg" alt="CLAI" width="128" height="128">
</p>

<h1 align="center">CLAI</h1>

<p align="center">
  <a href="https://opensource.org/licenses/MIT"><img src="https://img.shields.io/badge/License-MIT-green.svg" alt="License: MIT"></a>
  <a href="https://github.com/clairun/clai/releases"><img src="https://img.shields.io/github/v/release/clairun/clai" alt="GitHub release"></a>
  <a href="https://github.com/clairun/clai/actions/workflows/ci.yml"><img src="https://github.com/clairun/clai/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
</p>

<p align="center">
  A desktop app for building, running, and supervising small teams of AI agents.
</p>

<p align="center">
  <em>Local-first and provider-agnostic — your agents run on your machine, use your
  tools and files under per-agent permissions, and report to you from one Fleet cockpit.</em>
</p>

<p align="center">
  <a href="https://clai.run/"><img width="100%" alt="CLAI — a main agent delegating a review task to a Code Reviewer agent" src="docs/clai-hero.gif"></a>
  <br>
  <a href="https://clai.run/"><strong>▶ Watch the full demo</strong></a>
</p>

## What it is

CLAI organizes work into **workspaces**. A workspace is an ongoing
conversation with a **main agent** — a configurable LLM that owns the
workspace's provider, tools, skills, and permissions. The main agent can:

- **Chat** with you in a surface that stays front and center
- **Use tools** through attached MCP servers and a local filesystem/shell sandbox
- **Delegate** to helper agents in the same workspace, each with their own
  skills, tools, and execution policy
- **Persist context** — memories and artifacts it writes are inspectable from
  the workspace header
- **Run on a schedule** — periodic workspaces fire on their interval

The **Fleet** view supervises everything: scheduled workspaces float to the
top, anything needing attention (failed or blocked tasks, input prompts) is
flagged, and selecting a card slides in a live chat preview.

## Features

- **Workspace-local agent teams** — Add helpers (e.g. a Code Reviewer or SoW
  Tracker) with their own prompts, skills, MCP servers, providers, and
  execution policy. The main agent calls them as tools, and you can read their
  full transcripts.
- **Multiple providers** — API connections (OpenAI-compatible or
  Anthropic-compatible) and local CLI agents such as Claude Code, OpenAI
  Codex, and OpenCode. Each agent picks its own.
- **MCP-native tools** — Configure MCP servers once in Settings, then attach
  them per workspace or per agent. HTTP and stdio transports.
- **Local execution sandbox** — Per-agent filesystem grants and three shell
  modes: *Off*, *Restricted* (only allowed command prefixes — `kubectl get`
  permits `kubectl get pods` but not `kubectl delete`), and *Full*.
- **Default skills + templates** — CLAI registers the read-only
  `clairun/clai-skills` repository by default and ships agent templates
  (`code-reviewer`, `sow-tracker`) ready to drop in.
- **Inspectable tasks** — Delegated work streams a live transcript: the
  helper agent's full conversation, tool calls, and verdict.
- **Memory & artifacts** — Agents persist findings to the workspace directory;
  the drawer surfaces both, with read-only previews (rendered markdown, pretty
  JSON, multi-file HTML).
- **Run notices** — A run that hits a policy denial finishes in an amber
  "warnings" state and surfaces in the Fleet, instead of failing silently.

> **Local execution sandboxing is platform-specific.** On Linux, shell commands
> run through bubblewrap. On macOS, shell commands run through Seatbelt via
> `sandbox-exec`. On platforms without a backend, shell execution is labeled as
> a host shell. The allow/block lists still control *which commands* an agent can
> run; filesystem grants control what sandboxed shell commands and built-in
> `fs_*` tools can access.

## Install

Download the latest build for your platform from the
[Releases page](https://github.com/clairun/clai/releases):

| Platform    | Download |
|-------------|----------|
| Windows     | `.msi` / `.exe` |
| macOS       | `.dmg` |
| Linux       | `.deb`, `.rpm`, or `.flatpak` |
| Arch Linux  | AUR: [`clai-desktop-bin`](https://aur.archlinux.org/packages/clai-desktop-bin) |

## Getting started

1. **Add a provider** — In Settings, connect an API provider or point CLAI at
   a local CLI agent. Supported CLI agents are auto-detected and pre-wired into
   new workspaces.
2. **Add MCP servers (optional)** — Register local or remote MCP servers for
   external tools.
3. **Create a workspace** — From the Fleet view. Open its settings (gear icon
   in the header) to attach a provider, MCP servers, and skills.
4. **Chat** with the main agent.
5. **Add helper agents (optional)** — From the *Agents* drawer, pick a
   template or build one from scratch; the main agent can then delegate to it.
6. **Make it periodic (optional)** — Toggle *Schedule* to run the main agent
   on an interval.

## Providers

- **API connections** — Add an **OpenAI-compatible** or **Anthropic-compatible**
  provider with an API key and optional custom base URL. Works with OpenAI,
  Anthropic, together.ai, Groq, and local endpoints (vLLM, llama.cpp, Ollama).
- **CLI agents** — Drive a locally installed coding CLI directly, such as
  **Claude Code**, **OpenAI Codex**, or **OpenCode**. CLAI exposes its tools to
  them over MCP and streams their output like any other run.

Each agent chooses its own provider, so you can mix — for example a Claude
Code main agent with an OpenAI-compatible reviewer.

## Development

```bash
git clone https://github.com/clairun/clai.git
cd clai
npm install
make dev            # run the desktop app in development
```

Before pushing:

```bash
npm run lint && npm run format:check && npm run build
cargo fmt --manifest-path src-tauri/Cargo.toml --check
cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets -- -D warnings
cargo test --manifest-path src-tauri/Cargo.toml
```

## Architecture

- **Frontend** — React + Tauri; a chat-first workspace with a drawer for
  agents / tasks / memories / artifacts and slide-out transcript and file panels.
- **Runtime** — Each workspace owns one or more agent rows and a persistent
  session. Built-in tools (filesystem, inter-agent calls, task management) plus
  MCP tools, gated by each agent's policy.
- **Scheduler** — Periodic workspaces run from the agent runner, emitting the
  same streaming events as interactive chat.
- **Skills + templates** — Skills are discovered from read-only local or git
  sources. The app-managed default source is `clairun/clai-skills`; agent
  templates are embedded via `include_dir!`.

## License

MIT
