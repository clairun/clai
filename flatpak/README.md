# Flatpak

clai ships a side-loadable `.flatpak` bundle, built in CI after each
Release (`.github/workflows/flatpak.yml`). The job downloads the released
`.deb`, extracts the `clai` binary, and wraps it with `flatpak build-init`
/ `build-finish` against the GNOME 49 runtime.

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
flatpak build-init flatpak-build io.github.juacker.clai org.gnome.Sdk//49 org.gnome.Platform//49
# ...copy files (see the workflow for the exact layout)...
flatpak build-finish flatpak-build \
  --command=clai \
  --share=ipc --share=network \
  --socket=x11 --socket=wayland --device=dri \  # x11 (not fallback-x11): arboard image-clipboard needs XWayland
  --filesystem=home \
  --talk-name=org.freedesktop.secrets \
  --talk-name=org.freedesktop.Flatpak
flatpak build-export repo flatpak-build
flatpak build-bundle repo clai.flatpak io.github.juacker.clai
flatpak install --user clai.flatpak && flatpak run io.github.juacker.clai
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

- Side-loadable, not Flathub-ready. Flathub forbids network during build,
  so cargo/npm deps would need vendored dependency manifests, and the
  binary should be built inside the SDK (not copied from a `.deb`, to
  avoid a host/runtime glibc mismatch).
- The broad permissions above are intentional for the local-execution
  features and need review before any Flathub submission.
