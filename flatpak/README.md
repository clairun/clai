# Flatpak

clai ships a side-loadable `.flatpak` bundle, built in CI after each
Release (`.github/workflows/flatpak.yml`). The job downloads the released
`.deb`, extracts the `clai` binary, and wraps it with `flatpak build-init`
/ `build-finish` against the GNOME 49 runtime.

When the publishing secrets are configured (see below), CI additionally
maintains a **GPG-signed OSTree repo on R2**, served at
`https://download.clai.run/flatpak/repo`. Installs that use the repo get
normal `flatpak update` / GNOME Software update semantics.

## Installing / updating

Preferred — add the remote once, install from it, updates arrive like any
other Flatpak:

```bash
flatpak remote-add --user clai https://download.clai.run/flatpak/clai.flatpakrepo
flatpak install --user clai run.clai.CLAI
flatpak update       # picks up new CLAI releases
```

Side-loading the `clai.flatpak` bundle from GitHub Releases also works.
Bundles built after the repo went live embed `--repo-url` and the repo's
public key, so installing one **auto-attaches the `clai` origin remote**
(Chrome/VS Code style) and later releases arrive via `flatpak update` too.
Bundles from before the repo (no origin remote) can't update themselves;
the app falls back to its in-app notify-only update check for those.

## How publishing works (CI)

`flatpak.yml` degrades gracefully: everything in this section only runs
when ALL of `LINUX_REPO_GPG_PRIVATE_KEY`, `R2_ACCESS_KEY_ID`,
`R2_SECRET_ACCESS_KEY`, `R2_ACCOUNT_ID` are set as repository secrets.
Otherwise CI builds the plain unsigned side-load bundle exactly as before.

Per release:

1. Pull the previous OSTree repo from `s3://clai-releases/flatpak/repo`
   (empty on the first run; `build-export` initializes it).
2. `flatpak build-export --gpg-sign=…` appends the new release commit,
   `build-update-repo --generate-static-deltas` signs the summary and
   builds deltas for cheap updates.
3. `flatpak build-bundle --repo-url=… --gpg-keys=…` produces the
   side-load bundle that auto-attaches the remote.
4. Push the repo back to R2 — content-addressed `objects/` and `deltas/`
   first with immutable caching, then the mutable metadata (`summary`,
   `refs/`, `config`) with a 5-minute edge cache, so a client pulling
   mid-publish never sees refs pointing at missing objects. (`summary` /
   `summary.sig` are two separate uploads, so a client fetching in that
   brief window (extended at CDN edges by the 5-minute cache) can hit a
   transient signature-verify failure —
   retrying fixes it; fully atomic metadata swaps aren't possible on
   plain object storage.) Runs are serialized by the `flatpak-build`
   concurrency group, so pushes never race each other.
5. Regenerate and upload `flatpak/clai.flatpakrepo` (embeds the public
   key, so remotes added from it are GPG-verified).

## Repo signing key (maintainer)

The OSTree repo is signed with the shared CLAI Linux-repos key
(`CLAI Package Repo <packages@clai.run>`), stored ONLY as the
`LINUX_REPO_GPG_PRIVATE_KEY` secret. The same key signs the apt repo —
generation, rotation, and loss consequences are documented in
[packaging/linux-repo/README.md](../packaging/linux-repo/README.md).

CI derives the public key from the secret and embeds it in
`clai.flatpakrepo` on every publish, so Flatpak clients need nothing
committed here.

## How clai uses the host from inside the sandbox

clai is unusual for a Flatpak: it deliberately reaches the **host** for
two things, both through `flatpak-spawn --host`:

1. **AI provider CLIs** — `claude`, `codex`, `opencode`, etc. are the user's own
   host-installed tools; clai shells out to them on the host
   (`src-tauri/src/providers/mod.rs`).
2. **The `bash_exec` sandbox** — clai sandboxes agent shell commands with
   `bwrap`. Inside Flatpak, nested user namespaces are blocked by the
   outer sandbox's seccomp filter (and `bwrap` isn't in the runtime), so
   clai runs the **host's** bwrap via `flatpak-spawn --host bwrap …`
   (`src-tauri/src/assistant/sandbox/linux_bwrap.rs`). The sandbox profile
   and its security boundary are unchanged — bwrap just executes host-side.

Both require the Flatpak to hold **`--talk-name=org.freedesktop.Flatpak`**
(the host-spawn portal). The host must also have `bwrap` installed for
`bash_exec` to work (standard on most Linux desktops).

## Building locally

```bash
# Build a .deb first (matches what CI consumes), then:
ar x clai.deb && tar xf data.tar.*

flatpak install -y flathub org.gnome.Platform//49 org.gnome.Sdk//49
flatpak build-init flatpak-build run.clai.CLAI org.gnome.Sdk//49 org.gnome.Platform//49
# ...copy files (see the workflow for the exact layout)...
flatpak build-finish flatpak-build \
  --command=clai \
  --share=ipc --share=network \
  --socket=x11 --socket=wayland --device=dri \  # x11 (not fallback-x11): arboard image-clipboard needs XWayland
  --filesystem=home \
  --talk-name=org.freedesktop.secrets \
  --talk-name=org.freedesktop.Flatpak
flatpak build-export repo flatpak-build
flatpak build-bundle repo clai.flatpak run.clai.CLAI
flatpak install --user clai.flatpak && flatpak run run.clai.CLAI
```

## Verify after building (needs a real Flatpak install)

- [ ] App launches (WebKitGTK renders — the UI is a single-file bundle via
      `vite-plugin-singlefile`, required inside the sandbox).
- [ ] A provider CLI runs (send a message; confirm it isn't an
      "executable not found" / spawn error).
- [ ] `bash_exec` works: ask the agent to run a shell command and confirm
      it is NOT "Sandboxed shell is unavailable". This exercises
      `flatpak-spawn --host bwrap`.
- [ ] **Shared `~/.clai`**: `paths::clai_home()` and `expand_tilde()` now
      resolve the *real* host home under Flatpak (via
      `providers::get_home_dir()`, cached), so app config
      (`~/.clai/config.json`), skills, cache, and workspaces
      (`~/.clai/workspaces/…`) are shared with the native `.deb` install
      rather than isolated under `~/.var/app/…`. Confirm a workspace
      created in the `.deb` shows up in the Flatpak and vice-versa.
- [ ] **Agent `$HOME` reach** (separate from config): the default agent's
      filesystem grant and the sandbox profile's HOME env still derive
      from `dirs::home_dir()` (sandbox home). Confirm whether agents
      running host-side bwrap need these pointed at the real home too
      (e.g. to read `~/.gitconfig`, `~/.ssh`).

## Status / not-yet

- Not Flathub-ready. Flathub forbids network during build,
  so cargo/npm deps would need vendored dependency manifests, and the
  binary should be built inside the SDK (not copied from a `.deb`, to
  avoid a host/runtime glibc mismatch).
- The broad permissions above are intentional for the local-execution
  features and need review before any Flathub submission.
- The OSTree repo grows with each release (history is kept; objects are
  content-addressed so identical files dedupe). If size ever matters,
  add `flatpak build-update-repo --prune --prune-depth=N` plus
  `aws s3 sync --delete` — deliberately not done yet to keep publishes
  append-only and mid-pull-safe.
