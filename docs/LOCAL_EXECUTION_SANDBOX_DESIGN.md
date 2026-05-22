# Local Execution Sandbox Design

## Status

Draft for review.

## Problem

CLAI currently exposes filesystem grants and shell permissions as configuration,
but shell commands are still launched as the app user. The built-in filesystem
tools enforce path grants before reading or writing files, but `bash_exec` starts
a host shell with the user's normal OS permissions. If an agent is allowed to run
`cat`, `python`, `node`, `grep`, or any other general-purpose command, that
process can access any path the user can access.

This means the current configuration is an authorization policy, not an
isolation boundary. The goal is to make configured path grants technically true
for all model-triggered local execution.

## Goals

- Enforce filesystem grants at the operating-system process boundary.
- Keep LLM inference where it already is. API-backed models remain remote, and
  CLI-backed models remain local CLI processes when configured.
- Avoid requiring Docker, Podman, VM images, or a full container runtime.
- Preserve the current approval flow and command allowlists as a UX and policy
  layer.
- Fail closed when sandboxed execution is requested but unavailable.
- Start with a practical Linux implementation, then add platform-specific
  backends.

## Non-goals

- Do not run the LLM itself in a container.
- Do not attempt to make shell command parsing a complete security boundary.
- Do not grant broad host access just because a command prefix was approved.
- Do not solve every platform in the first iteration.
- Do not provide a user-facing option to disable sandboxing on platforms where
  a backend exists. Sandboxing is a property of the platform build, not a
  runtime setting.

## Reference Model

This design follows the shape used by Codex local execution:

- The model proposes actions.
- A local harness decides what can run.
- Commands run inside an OS-enforced sandbox.
- Approvals decide whether to allow actions that need policy exceptions.

OpenAI's public descriptions of Codex local sandboxing are useful references:

- Codex local uses OS facilities such as Seatbelt on macOS, seccomp or
  bubblewrap on Linux, and a custom Windows command runner.
- The sandbox is the technical boundary for filesystem writes, network access,
  and protected paths.
- Approval policy is a separate layer for actions that cross or request changes
  to that boundary.

References:

- https://openai.com/index/building-codex-windows-sandbox/
- https://openai.com/index/running-codex-safely/
- https://developers.openai.com/codex/cloud

## Current State

Relevant current implementation points:

- `src-tauri/src/assistant/tools/local.rs`
  - `execute_bash_exec` checks shell mode and command policy, resolves `cwd`,
    then spawns `/bin/sh -lc <command>`.
  - Filesystem helpers resolve paths against workspace grants before using
    `fs_read`, `fs_write`, `fs_list`, and `fs_glob`.
- `src-tauri/src/assistant/local_agent.rs`
  - CLI-backed Claude Code runs as a child process of CLAI.
  - The CLI is launched with `--strict-mcp-config` (user-installed MCP servers
    are not loaded), `--mcp-config` pointing at a CLAI-provisioned config,
    `--permission-mode bypassPermissions` (CLAI's tools route through MCP, so
    the CLI's prompt layer is bypassed), and `--disable-slash-commands`.
  - Native Claude tools are restricted via `--disallowedTools` enumerating
    every known built-in (`Bash,Read,Edit,Write,Glob,Grep,WebFetch,WebSearch,
    Task,TodoWrite,NotebookEdit,NotebookRead,LSP`). This is a denylist and
    requires maintenance whenever Anthropic adds a tool. Switching to the
    CLI's allowlist primitive is part of the v1 work below.
  - The CLI inherits `HOME`, `XDG_CONFIG_HOME`, and the parent working
    directory. User-defined `~/.claude/settings.json` hooks and `CLAUDE.md`
    files from cwd and parent directories are therefore visible to the CLI
    process. Isolating the CLI's config directory is part of the v1 work
    below.
  - The CLI process itself runs with the user's normal OS permissions. This
    is by design (the user installed and authed the CLI); CLAI's
    responsibility is limited to the tool surface the model can reach
    through the CLI, not the CLI's own host-side behavior.
- `src-tauri/src/assistant/engine.rs`
  - The system prompt currently documents the filesystem boundary as a soft
    contract because shell access can escape it.

The design below replaces that soft contract with an enforced process boundary.

## Architecture

Introduce a small execution abstraction:

```text
Model/tool call
  -> tools::router
  -> CLAI-controlled local tool execution (for example bash_exec)
  -> SandboxedCommandRunner
  -> platform sandbox backend
  -> child process and descendants
```

The important property is that every model-triggered process launched by
CLAI-controlled local tools starts inside the sandbox, and every descendant
remains inside the same boundary.

CLI-backed provider processes are the exception to this flow. The provider CLI
itself is a host process because it is user-installed, user-authenticated, and
owns its own config, cache, credential, and telemetry behavior. CLAI's boundary
for CLI-backed providers is the tool surface exposed to the model: the model
must only see CLAI-provisioned MCP tools, and any local execution performed by
those tools must route through `SandboxedCommandRunner`.

### New module

Add a Rust module, for example:

```text
src-tauri/src/assistant/sandbox/
  mod.rs
  profile.rs
  runner.rs
  linux_bwrap.rs
  unsupported.rs
```

Core types:

```rust
pub struct SandboxProfile {
    pub workspace_root: PathBuf,
    pub path_grants: Vec<SandboxPathGrant>,
    pub network: SandboxNetworkMode,
    pub env: SandboxEnv,
}

pub struct SandboxPathGrant {
    pub host_path: PathBuf,
    pub access: SandboxPathAccess,
}

pub enum SandboxPathAccess {
    ReadOnly,
    ReadWrite,
}

pub enum SandboxNetworkMode {
    Disabled,
    Host,
}
```

`SandboxProfile` is derived from `ExecutionCapabilityConfig` plus the agent
workspace root.

### Execution contract

`SandboxedCommandRunner` should support:

- command argv
- working directory
- stdin mode
- stdout/stderr capture
- timeout
- max output size
- cancellation
- sandbox profile

It should return the same shape that `bash_exec` already returns:

- resolved `cwd`
- exit code
- success boolean
- stdout
- stderr

## Linux Backend

First implementation: `bubblewrap` (`bwrap`).

This avoids a full container runtime while still using Linux namespaces and bind
mounts. It is also conceptually close to the documented Codex Linux approach.

### Basic sandbox layout

For a command running in an agent workspace:

- **System binds (read-only).** A fixed baseline that makes a standard shell
  and common toolchains usable: `/usr`, `/etc`, and (where present on the
  host) `/bin`, `/sbin`, `/lib`, `/lib32`, `/lib64`, `/libx32`, and `/sys`.
  `/etc` is bound whole; the information-disclosure surface is low because the
  sandboxed process runs as the user.
- **Pseudo-filesystems.** `--proc /proc` (safe because `--unshare-pid` gives
  the sandbox its own PID namespace), `--dev /dev` (bwrap-minimal `/dev`
  with `null`, `zero`, `urandom`, `tty`, and the stdin/stdout/stderr
  symlinks), `--tmpfs /tmp` (private to the sandbox).
- **Runtime paths and sockets.** Do not bind `/run` wholesale. Even a read-only
  bind can expose Unix sockets such as session buses, ssh-agent, gpg-agent,
  keyrings, portals, container engines, or systemd user services, and filesystem
  read-only status does not prevent connecting to a socket. If a distro layout
  requires runtime files for basic operation, expose only specific file paths
  after resolving symlinks (for example resolver files), never socket
  directories. Concretely: at launch, resolve symlinks under `/etc` that point
  into `/run` (notably `/etc/resolv.conf` on systemd-resolved hosts), create
  only the needed private parent directories inside the sandbox with `--dir`,
  and `--ro-bind` each resolved target *file* at its expected in-sandbox path.
  Never bind the host parent directory just to make the destination path
  exist. Apply the same treatment to `/etc/localtime` when it points outside
  `/usr`. This is what the "runtime path allowlist generation" unit test
  covers.
- **`/sys` is also a coarse bind** with the same information-disclosure
  framing as `/etc`. It exposes hardware topology, kernel parameters, and
  cgroup info; writes are denied by the read-only bind. Pruning is fiddly
  and breaks tools like `lscpu` and `lsblk`, so the pragmatic call is to
  bind whole and accept the read-only info disclosure.
- **`/dev/shm` is not in the bwrap-minimal `/dev`.** Some toolchains
  (Chromium, certain Node packages, several Python ML libs) require it.
  Add `--tmpfs /dev/shm` on demand rather than expanding the baseline;
  v1 ships without it and documents this as a known issue.
- **Workspace and grants.** Bind the agent workspace read-write at its real
  absolute path. Bind each `extraPaths` grant at its real absolute path:
  `ReadOnly` as `--ro-bind`, `ReadWrite` as `--bind`. Paths are exposed
  using real host paths, not virtual `/workspace` aliases, so the LLM and
  the user share one coordinate system.
- **HOME and cwd.** Do not bind the user's real `$HOME`. Set `HOME` to the
  agent workspace (or an in-sandbox path under it). Set cwd to the resolved
  sandbox cwd.
- **Environment.** Drop ambient host environment except an explicit
  allowlist (`PATH`, `LANG`, `LC_*`, `TZ`, `TERM`, plus what the sandbox
  itself sets). Independently of the allowlist, treat the following
  socket-locating and display-related variables as an explicit deny set
  that must never reach the sandbox even if a future contributor widens
  the allowlist: `SSH_AUTH_SOCK`, `SSH_AGENT_PID`, `DBUS_SESSION_BUS_ADDRESS`,
  `DBUS_SYSTEM_BUS_ADDRESS`, `XDG_RUNTIME_DIR`, `DOCKER_HOST`,
  `CONTAINER_HOST`, `PODMAN_HOST`, `WAYLAND_DISPLAY`, `DISPLAY`,
  `XAUTHORITY`, `GPG_AGENT_INFO`, `GPG_TTY`, `GIT_ASKPASS`, `SSH_ASKPASS`,
  `SUDO_ASKPASS`. Stripping these removes several direct paths back to host
  services even when the corresponding socket paths are not bind-mounted (some
  addresses are abstract or guessable, see §Network).
- **Network.** Shared with the host by default. Per-agent
  `sandbox.network: disabled` switches to a fully unshared network
  namespace (no interfaces at all, not even loopback). See §Network below.

Example shape, not final argv:

```text
bwrap
  --unshare-user
  --unshare-ipc
  --unshare-pid          # required for --proc to be safe; also enables teardown
  --unshare-uts
  --unshare-cgroup
  --share-net            # default; omit to deny network per-agent
  --die-with-parent
  --new-session
  --proc       /proc
  --dev        /dev
  --tmpfs      /tmp
  --ro-bind     /usr        /usr
  --ro-bind-try /bin        /bin
  --ro-bind-try /sbin       /sbin
  --ro-bind-try /lib        /lib
  --ro-bind-try /lib32      /lib32
  --ro-bind-try /lib64      /lib64
  --ro-bind-try /libx32     /libx32
  --ro-bind     /etc        /etc
  --ro-bind-try /sys        /sys
  --bind        <workspace> <workspace>
  --ro-bind     <read-only-grant>  <read-only-grant>
  --bind        <read-write-grant> <read-write-grant>
  --setenv HOME <workspace>
  --chdir       <cwd>
  /bin/sh -lc <command>
```

Distros and packaging layouts that put binaries outside this baseline
(notably NixOS, where binaries live under `/nix/store/...`) need
additional binds. Document those hosts as known limitations rather than
expanding the baseline.

### Process teardown

Teardown is structural, not a separate resource-management layer:

- `--unshare-pid` makes the bwrap process PID 1 of a new PID namespace.
- `--die-with-parent` ensures the kernel kills bwrap when CLAI (its
  parent) exits or kills it.
- When PID 1 of a namespace dies, the kernel reaps every descendant in
  that namespace.

The practical consequence: a single `child.kill()` from CLAI on the bwrap
process is sufficient to tear down the entire sandboxed process tree on
timeout or cancellation, including detached children and processes that
called `setsid()`. No cgroup machinery is required for correctness.

### Resource limits

CLAI does not impose special resource limits on sandboxed processes. The
sandboxed shell inherits the same user-level `RLIMIT_*` values that any
other process spawned by the user already has (typically set by
`pam_limits` or the systemd user slice). Forking aggressively or
allocating heavily from inside the sandbox runs into the same caps that
already apply to any process the user runs by hand — neither tighter
nor looser. The only in-sandbox runtime policy CLAI enforces is
`bash_exec`'s existing `timeoutMs`, which bounds wall-clock runtime.

### Network

Default: network is shared with the host. The sandboxed process can
clone repos, fetch packages, reach the public internet, and reach
services on localhost — the same network surface the user's normal
shell has. This is a friction-reduction default; the alternative
(deny-by-default with per-command approval) is too noisy for common
workflows like `git clone`, `pip install`, `npm install`.

Per-agent `sandbox.network: disabled` adds `--unshare-net` to the
sandbox, leaving the process with no network interfaces at all (not
even loopback). Use this for agents operating on workspaces that
contain secrets or other data the user does not want exfiltrable.

Optional domain/proxy policy is out of scope for v1.

`network: disabled` is a network-namespace policy, not an IPC policy. It does
not protect the host if the sandbox bind-mounts Unix sockets from `/run`,
`/tmp`, or a granted path. The mount baseline therefore must keep host runtime
socket paths out of the sandbox unless the user explicitly grants the owning
directory and accepts that consequence.

Abstract Unix sockets (`unix:abstract=...`) are network-namespace-scoped, so
`network: disabled` blocks them as a side effect of `--unshare-net`. Some
host services use path sockets under `/run`; others, including some D-Bus and
display-server configurations, can use abstract sockets. With the default
network-shared sandbox, abstract sockets in the host's network namespace remain
reachable, which is why the environment denylist strips `DBUS_SESSION_BUS_ADDRESS`,
`DISPLAY`, and similar variables even though no `/run` paths are bound.
Without the address, the sandboxed process must guess the socket name and still
needs whatever service-level credential the host service requires.

**Limit of the boundary.** With network shared by default, the sandbox
protects host **filesystem integrity** outside the configured grants —
it does **not** protect data **confidentiality** inside the grants.
Anything the sandboxed process can read (workspace files, RW-granted
paths, including `.env`-style secrets that often live in workspace
dirs) can be sent over the network. Use `network: disabled` per agent
when this matters.

### Unsupported hosts

Hosts where unprivileged user namespaces are disabled
(`kernel.unprivileged_userns_clone=0`, common on older RHEL and some
hardened Debian configurations) cannot run bwrap unprivileged. On such
hosts, `bash_exec` is unavailable and the agent settings UI surfaces
this with a sysctl remediation pointer. CLAI does not support these
hosts in v1; no workaround is provided. Detection must distinguish
"bwrap missing" (install the package) from "bwrap installed but kernel
forbids namespaces" (sysctl issue).

## macOS Backend

Target backend: Seatbelt via `sandbox-exec` style profiles or a native wrapper.

macOS can enforce filesystem read/write policy at process level. The backend
should map `SandboxProfile` to:

- allow read/write for workspace and writable grants
- allow read-only for read-only grants
- deny access to protected paths by default
- optionally deny network by default

This is a second phase. Until implemented, macOS runs `bash_exec` as today
without any additional sandbox layer. There is no user-facing toggle and no
soft-fallback prompt: the platform either has a backend (label: "Sandboxed
shell") or it does not (label: "Host shell — sandbox not yet available on
this platform"). When the macOS backend lands, it becomes mandatory the same
way Linux's is mandatory in v1.

## Windows Backend

Target backend: separate command runner using a low-privilege sandbox account or
restricted token model.

OpenAI's Codex Windows writeup is relevant here: they split execution into a
main app, elevated setup helper, sandbox command runner, and child process.
That is likely too much for CLAI's first iteration, but the design point is
important: Windows probably needs a dedicated command runner binary rather than
trying to bolt restrictions onto a normal child process.

Windows should not block the Linux implementation. Treat it as a later backend
under the same rule as macOS: v1 runs `bash_exec` as today, labeled "Host
shell — sandbox not yet available on this platform." When the Windows
backend lands it becomes mandatory.

## Flatpak

CLAI's Flatpak build presents a specific structural problem: the Flatpak
sandbox is **per-application**, not per-workspace. Multiple agents inside
one Flatpak'd CLAI share the same Flatpak sandbox; the Flatpak boundary
alone cannot enforce per-workspace path grants. The inner bwrap layer is
therefore required, not optional, on Flatpak — it is the mechanism that
makes workspace grants real even when the outer Flatpak is permissive.

### bwrap inside Flatpak

CLAI bundles a statically-built `bwrap` binary inside the Flatpak rather
than relying on the host's installation or on `flatpak-spawn`. This keeps
the sandbox path self-contained: same `SandboxProfile`, same backend
implementation, same code path on native Linux and on Flatpak. No
additional Flatpak D-Bus permissions are required.

This depends on the kernel allowing nested unprivileged user namespaces,
which modern kernels do by default. Hosts under §Unsupported hosts above
are unsupported on Flatpak for the same reason they are unsupported on
native Linux.

### Flatpak permissions

Per-workspace path grants must be subsets of the Flatpak's declared
filesystem permissions. If the manifest declares `--filesystem=home`,
the user can grant any path under their home to a workspace; granting
`/opt/external` fails at configuration time with a clear message about
the Flatpak's filesystem scope rather than silently failing at run time.

Recommended Flatpak manifest baseline:

- `--filesystem=home` — broad enough to cover typical project workflows.
- `--share=network` — needed for the default network-allowed sandbox.
- Whatever portal/D-Bus permissions CLAI's UI already requires.

No `--talk-name=org.freedesktop.Flatpak.Development` and no use of
`flatpak-spawn --sandbox` — sandboxing is handled by the bundled bwrap.

### Implementation spike

Before locking the Flatpak manifest, run a small spike that confirms:

1. The bundled `bwrap` runs from inside the Flatpak.
2. Nested user namespaces work in the target Flatpak runtime
   (`org.freedesktop.Platform` 23.08 or newer).
3. Per-workspace bind mounts resolve correctly when paths are subsets of
   the Flatpak's filesystem permissions.

If any of those fail, the fallback option is `flatpak-spawn --sandbox`
via the Flatpak Development D-Bus interface, which requires the
corresponding manifest permission. Treat this as plan B; plan A
(bundled bwrap) is the default direction.

## Shell Policy vs Sandbox Policy

The current command allowlist and blocklist should remain.

However, their meaning should be clarified:

- Command policy decides whether a command may be attempted.
- Sandbox policy decides what the command and descendants can access.

Approving `cat` should not imply access to `~/.ssh`. It only means `cat` can run
inside the configured sandbox.

If a command needs a path outside the sandbox, the agent should request a new
path grant. The user can approve that grant explicitly, and subsequent command
execution gets a new `SandboxProfile`.

## Built-in Filesystem Tools

The built-in `fs_*` tools should continue to enforce grants in-process, but they
also need hardening:

- Canonicalize existing paths before access.
- Reject symlink escapes from granted roots.
- Be careful with write paths where the final file may not exist yet.
- Prefer platform APIs that avoid time-of-check/time-of-use races where
  practical.

This is separate from shell sandboxing but should be part of the same security
work because a symlink escape would undermine the configured path model.

## CLI-backed Providers

CLAI's responsibility for CLI-backed providers (Claude Code, Codex CLI, etc.)
is the tool surface the model can reach through the CLI, not the CLI's own
process behavior. The CLI is a host process the user installed and
authenticated; its own filesystem touches (config files, caches, session
state, auto-memory, auth tokens, telemetry) are the vendor's contract with
the user, accepted at install time. CLAI does not try to sandbox the CLI
process itself.

The model running through the CLI must see exactly CLAI's MCP-provisioned
tools — no more, no less. That property is enforced by four launch
invariants:

1. **Tool-surface allowlist.** Launch the CLI with its allowlist primitive
   set to exactly the CLAI MCP tool names. Tools added by the CLI vendor in
   future releases are excluded by default; no maintenance is required as
   new built-ins ship. This replaces today's `--disallowedTools` denylist,
   which requires updating every time Anthropic adds a tool.
2. **Isolated config directory.** Launch the CLI with `HOME` /
   `XDG_CONFIG_HOME` / vendor-specific config env (`CLAUDE_CONFIG_DIR` or
   equivalent) pointed at a CLAI-managed directory containing only the MCP
   config CLAI provisions. The user's `~/.claude/settings.json` hooks,
   `CLAUDE.md` files, and globally installed MCP servers must not be
   reachable from this directory. Auth credentials are the one exception:
   bind only the specific credential file(s) needed for the CLI to
   authenticate, read-only, into the isolated config directory.
3. **Controlled working directory.** Launch with cwd set to the agent
   workspace, never the user's `$HOME` or CLAI's own cwd. This prevents
   implicit ingestion of project files outside the workspace.
4. **Startup tool-inventory assertion.** Immediately after launch, query
   the tool list the CLI advertises to the model and assert it equals the
   expected CLAI MCP set exactly. If anything else is advertised — a new
   vendor built-in that slipped past the allowlist, an MCP server that
   loaded unexpectedly — fail closed.

`--strict-mcp-config` (already used) supports invariant 1 by neutralizing
auto-loaded MCP servers. `--disable-slash-commands` (already used) removes a
host-side input channel. `--permission-mode bypassPermissions` (already
used) is acceptable because CLAI's tools route through MCP and are
sandboxed there, not via the CLI's prompt layer.

With invariants 1–4 in place, the model has no path to disk that bypasses
CLAI's MCP tool surface, and the sandboxed MCP tools (Linux: bwrap;
elsewhere: in-process for `fs_*`, host shell for `bash_exec` until the
platform backend ships) are the actual boundary. The CLI process can still
read and write its own config and credentials, but the model cannot direct
it to do so in CLAI-unintended ways.

This is a v1 security work item. It is independent of the bwrap effort and can
ship in parallel, but CLAI should not claim CLI-backed providers have the same
tool-surface boundary until these invariants are implemented and tested.

## Configuration Model

Add an execution sandbox setting:

```json
{
  "execution": {
    "sandbox": {
      "network": "enabled"
    },
    "filesystem": {
      "extraPaths": [
        { "path": "/repo", "access": "readWrite" },
        { "path": "/docs", "access": "readOnly" }
      ]
    },
    "shell": {
      "mode": "restricted"
    }
  }
}
```

There is no `mode` field. Sandbox availability is a property of the platform
build, not a runtime setting:

- A platform either has a backend (Linux v1: bwrap) or it does not (macOS v1,
  Windows v1).
- Where a backend exists, every CLAI-controlled local tool execution runs
  through it, always, with no fallback and no opt-out.
- Where no backend exists yet, execution runs as today and the UI labels it
  accordingly. No mid-session prompt asks the user to accept unsandboxed
  execution.

The `network` field is in-sandbox policy: it controls whether a sandboxed
command may reach the network. On platforms without a backend, it has no
effect (the host shell can already reach the network as the user can).

Recommended default for agents:

- `sandbox.network = enabled` — clones, package installs, and fetches
  work without prompts. Flip to `disabled` per agent for workspaces that
  contain secrets (see §Network for the confidentiality caveat).
- `shell.mode = restricted`

## UX Changes

Agent settings communicate three separate concepts:

- Filesystem grants: which paths the agent can access.
- Shell approval: which commands can be run without prompting.
- Sandbox status: a platform-level fact about whether the OS is enforcing
  the path boundary. Not user-configurable.

Two platform-fact labels, exactly:

- `Sandboxed shell` — a backend is available and enforcing the boundary.
- `Host shell — sandbox not yet available on this platform` — no backend
  ships for this OS in this CLAI version.

Plus an in-sandbox policy label when sandboxed:

- `Network disabled` / `Network allowed for this agent`.

When the platform has no backend, the UI states that fact once at run start
(banner or notice) and does not offer to "run unsandboxed anyway" — there
is nothing to offer; the platform simply does not have the capability yet.

When the platform has a backend but a runtime dependency is missing (e.g.,
Linux without `bwrap` installed), the affected capability (`bash_exec`) is
shown as unavailable with a clear install instruction. There is no fallback
to host shell.

## Migration Plan

1. Add `SandboxProfile` and a no-op backend for non-Linux platforms.
2. Change `bash_exec` to call `SandboxedCommandRunner`.
3. Implement Linux `bwrap` backend per §Linux Backend (system-path
   baseline with no wholesale `/run` bind, real-absolute-path grants,
   PID-namespace teardown, network-shared by default).
4. Add runtime detection for `bwrap` on Linux. If absent, mark `bash_exec`
   as an unavailable capability with an install instruction surfaced in the
   agent settings UI. Distinguish "bwrap missing" from "kernel disallows
   unprivileged namespaces." Do not fall back to host shell.
5. Add UI status for sandbox availability (banner/notice at run start and
   in the agent settings panel). Land this together with step 3 so users
   can see why a command failed.
6. Update README to replace "local execution is not sandboxed" with the
   platform-specific status. Land this together with step 3.
7. Tighten CLI-backed provider launch as a security milestone (see
   §CLI-backed Providers): switch
   from `--disallowedTools` (denylist) to the CLI's allowlist primitive;
   isolate the CLI's `HOME` / config directory so user hooks and
   `CLAUDE.md` files are not inherited; set cwd to the agent workspace;
   assert at startup that the advertised tool inventory matches the
   expected CLAI set, failing closed otherwise. Independent of the bwrap
   work; can ship in parallel.
8. Add tests for path isolation (see §Testing Strategy).
9. Run the Flatpak bwrap-bundling spike (see §Flatpak). On success,
   update the Flatpak manifest to bundle a statically-built `bwrap` and
   declare the `--filesystem=home` + `--share=network` baseline. On
   failure, fall back to `flatpak-spawn --sandbox` with the
   corresponding manifest permission.

## Testing Strategy

Unit tests:

- profile derivation from `ExecutionCapabilityConfig`
- grant normalization
- cwd validation
- env filtering
- runtime path allowlist generation

Integration tests on Linux:

- run `pwd`, `ls`, `cat`, `touch`, and `python` inside the sandbox
- verify denied host paths are inaccessible
- verify read-only grants reject writes
- verify writes cannot escape through symlinks
- verify generated symlinks and nested grants cannot escape their intended
  access level
- verify host Unix sockets under common runtime paths are not reachable by
  default (`XDG_RUNTIME_DIR`, session bus, ssh-agent, gpg-agent,
  Docker/Podman, systemd user sockets)
- verify socket-related environment variables are stripped or rewritten
  (`SSH_AUTH_SOCK`, `DBUS_SESSION_BUS_ADDRESS`, `XDG_RUNTIME_DIR`,
  `DOCKER_HOST`, `CONTAINER_HOST`, `DISPLAY`, `WAYLAND_DISPLAY`,
  `XAUTHORITY`)
- verify `network: disabled` blocks TCP/UDP access and does not leave an
  accidental Unix-socket path back to host services
- verify host PIDs are not visible inside the sandbox (a fresh `/proc`
  under `--unshare-pid` shows only in-namespace PIDs; this guards against
  a bwrap version regression that would leak host PIDs)
- verify abstract-socket network on/off symmetry: with network shared, a
  test abstract listener on the host is reachable from inside the
  sandbox; with `network: disabled`, the same listener is unreachable
  (validates the abstract-sockets-are-netns-scoped claim in §Network)
- verify child processes inherit restrictions
- verify timeout and cancellation kill the sandboxed process tree

Manual test matrix:

- normal repo build
- test suite execution
- DNS on systems where `/etc/resolv.conf` points into `/run`
- package manager with network disabled
- package manager with network explicitly enabled
- CLI provider auth smoke test
- Flatpak packaging behavior

## Open Questions

- Do workspace-level permissions need a versioned schema for sandbox
  settings? Probably yes once `extraPaths` and `network` become
  user-facing, but the versioning shape is not urgent for v1.

## Recommendation

On Linux, route all CLAI-controlled model-triggered shell execution through
bwrap. bwrap is a runtime requirement; without it, `bash_exec` is unavailable
as a capability and the UI says so. On macOS and Windows in v1, `bash_exec`
runs as today with the UI labeling it accurately; when per-platform backends
ship they become mandatory the same way Linux's is.

In parallel, tighten CLI-backed provider launch: allowlist the model's tool
surface to exactly CLAI's MCP-provisioned tools, isolate the CLI's `HOME` /
config directory so no user-defined hooks or `CLAUDE.md` files are
inherited, set cwd to the agent workspace, and assert at startup that the
advertised tool inventory matches the expected set. The CLI process itself
remains unsandboxed and the CLI vendor owns its own host-side behavior; CLAI
is responsible only for the tool surface the model can reach.

Keep the current command allowlist/blocklist and approval flow, but stop
presenting it as the security boundary. The sandbox is the boundary; the
command policy is convenience and UX.

This gives CLAI a Codex-like safety model without requiring Docker or
running LLMs in containers.
