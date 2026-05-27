# Flathub Submission RFC

## Status
Draft — app works in Flatpak, submission pending (waiting for more features first).

## Context
CLAI runs correctly as a Flatpak both from source (SDK build) and from .deb extraction.
The critical ES module CORS issue in WebKitGTK was fixed using `vite-plugin-singlefile`
to inline all JS/CSS into a single HTML file (see commit `9cb5d51`).

## What's Done
- App ID: `io.github.juacker.clai`
- Metainfo: `flatpak/io.github.juacker.clai.metainfo.xml` — passes `appstreamcli validate`
- Desktop file: `flatpak/io.github.juacker.clai.desktop`
- CI Flatpak workflow: `.github/workflows/flatpak.yml` — builds from .deb, uploads to release
- Local from-source build: tested and working with GNOME 49 runtime
- Vite singlefile bundle: required for WebKitGTK compatibility in Flatpak sandbox

## What's Remaining for Flathub Submission

### 1. Generate Offline Dependency Manifests
Flathub builds have no network access. All dependencies must be pre-fetched.

```bash
# Install generators
pip install flatpak-node-generator  # use official from github.com/flatpak/flatpak-builder-tools
python3 flatpak-cargo-generator.py src-tauri/Cargo.lock -o cargo-sources.json

# Generate npm sources (use the official repo, NOT the pip package)
git clone --depth=1 https://github.com/flatpak/flatpak-builder-tools
cd flatpak-builder-tools/node
pip install -e .
flatpak-node-generator npm package-lock.json -o node-sources.json
```

These JSON files go in the Flathub fork only, not in this repo.

### 2. Create Flathub Manifest
The manifest must use offline sources. Template:

```yaml
id: io.github.juacker.clai
runtime: org.gnome.Platform
runtime-version: '49'
sdk: org.gnome.Sdk
sdk-extensions:
  - org.freedesktop.Sdk.Extension.node20
  - org.freedesktop.Sdk.Extension.rust-stable
command: clai

finish-args:
  - --share=ipc
  - --share=network
  - --socket=fallback-x11
  - --socket=wayland
  - --device=dri
  - --filesystem=home
  - --talk-name=org.freedesktop.secrets

build-options:
  append-path: /usr/lib/sdk/node20/bin:/usr/lib/sdk/rust-stable/bin
  env:
    CARGO_HOME: /run/build/clai/cargo
    XDG_CACHE_HOME: /run/build/clai/cache

modules:
  - name: clai
    buildsystem: simple
    sources:
      - type: git
        url: https://github.com/juacker/clai.git
        tag: vX.Y.Z
        commit: <sha>
      - node-sources.json
      - cargo-sources.json
    build-commands:
      - npm install --offline --ignore-engines --cache=flatpak-node/npm-cache
      - npx tauri build --no-bundle
      - install -Dm755 src-tauri/target/release/clai ${FLATPAK_DEST}/bin/clai
      - install -Dm644 flatpak/io.github.juacker.clai.desktop ${FLATPAK_DEST}/share/applications/io.github.juacker.clai.desktop
      - install -Dm644 flatpak/io.github.juacker.clai.metainfo.xml ${FLATPAK_DEST}/share/metainfo/io.github.juacker.clai.metainfo.xml
      - install -Dm644 src-tauri/icons/128x128.png ${FLATPAK_DEST}/share/icons/hicolor/128x128/apps/io.github.juacker.clai.png
      - install -Dm644 public/icon.svg ${FLATPAK_DEST}/share/icons/hicolor/scalable/apps/io.github.juacker.clai.svg
      - install -Dm644 LICENSE ${FLATPAK_DEST}/share/licenses/io.github.juacker.clai/LICENSE
```

### 3. Add `flathub.json`
```json
{
  "only-arches": ["x86_64"]
}
```

### 4. Linter Check
```bash
flatpak run --filesystem=/tmp --command=flatpak-builder-lint \
  org.flatpak.Builder manifest /path/to/io.github.juacker.clai.yml
```

Expected remaining warning: `finish-args-home-filesystem-access` — justified because
users configure per-agent filesystem access to arbitrary directories.

### 5. Update Metainfo
Before submission, update `io.github.juacker.clai.metainfo.xml` with:
- Latest release version and description
- Additional screenshots (Flathub displays them on the app page)

### 6. Submit PR
1. Fork [flathub/flathub](https://github.com/flathub/flathub/fork) — uncheck "Copy master branch only"
2. `git clone --branch=new-pr git@github.com:juacker/flathub.git`
3. `git checkout -b io.github.juacker.clai new-pr`
4. Add: `io.github.juacker.clai.yml`, `flathub.json`, `node-sources.json`, `cargo-sources.json`
5. Push and open PR titled **"Add io.github.juacker.clai"** against the `new-pr` branch
6. Comment `bot, build` to trigger test builds

**Important:** Per Flathub's Generative AI policy, the submission PR must not be AI-generated.
Review and understand all files before submitting.

### 7. Each New Release
After each release, update the Flathub repo:
1. Update `tag` and `commit` in the manifest
2. Regenerate `node-sources.json` and `cargo-sources.json` if dependencies changed
3. Push to the Flathub repo — it rebuilds and publishes automatically

## Key Technical Notes

### ES Module CORS Issue
Tauri's custom protocol (`tauri://localhost`) doesn't return CORS headers inside Flatpak's
WebKitGTK sandbox. ES modules (`<script type="module">`) always require CORS, so they
silently fail to load. Fix: `vite-plugin-singlefile` inlines everything into HTML.
Related: tauri-apps/tauri#8970.

### From-Source Build Works
Despite earlier debugging suggesting the SDK build was broken, the actual issue was always
ES modules. With `vite-plugin-singlefile`, from-source builds in the Flatpak SDK work
correctly. This is the preferred approach for Flathub.

### Runtime
GNOME 49 runtime tested and working. GitButler (another Tauri app on Flathub) also uses 49.

### Permissions Justification
- `--filesystem=home`: Users configure per-agent filesystem access to arbitrary directories
  via the "Additional Path Grants" UI. Not a sandboxing guarantee — documented in README.
- `--talk-name=org.freedesktop.secrets`: Required for API key storage via system keyring.
