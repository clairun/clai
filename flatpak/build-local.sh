#!/usr/bin/env bash
#
# Build a side-loadable clai .flatpak entirely on localhost — no push, no
# release. The binary is compiled INSIDE the GNOME SDK (rust-stable + node20
# extensions) so it links the runtime's glibc; a host build on a newer distro
# (e.g. Arch glibc 2.43) would pull symbols the GNOME 49 runtime (2.42) lacks
# and fail to start. Mirrors .github/workflows/flatpak.yml from that point on.
#
# Usage:
#   flatpak/build-local.sh            # build clai.flatpak in repo root
#   flatpak/build-local.sh --install  # also `flatpak install --user` it
#
set -euo pipefail

APP_ID=io.github.juacker.clai
SDK=org.gnome.Sdk//49
PLATFORM=org.gnome.Platform//49
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TARGET_DIR="$ROOT/src-tauri/target/flatpak"   # isolated from host-rustc target/
BIN="$TARGET_DIR/release/clai"
RUNTIME_GLIBC_MAJOR=2
RUNTIME_GLIBC_MINOR=42                          # GNOME 49 == freedesktop 25.08

# Stage the Flatpak build tree + OSTree repo OUTSIDE the working tree.
# `flatpak build-init` lays down an app filesystem skeleton that includes
# `var/run -> /run` — an absolute symlink to the host runtime dir. Kept in
# the repo, symlink-following tools (Vite's watcher and dep-scanner, etc.)
# crawl through it into the host's live /run and crash. A cache dir keeps the
# repo clean; both are throwaway and rebuilt from scratch each run.
BUILD_CACHE="${XDG_CACHE_HOME:-$HOME/.cache}/clai-flatpak"
BUILD_TREE="$BUILD_CACHE/build"
OSTREE_REPO="$BUILD_CACHE/repo"

cd "$ROOT"

# Build the frontend AND the production binary inside the SDK via the Tauri
# CLI (`tauri build`), NOT a bare `cargo build`. The CLI is what flips Tauri
# into production mode so `generate_context!` embeds dist/ and the webview
# loads the bundled assets via the tauri:// protocol. A plain `cargo build`
# (even --release) leaves it pointing at devUrl (http://localhost:1420), so
# the window shows "Could not connect to localhost: Connection refused".
# `--no-bundle` stops after the binary so we don't need dpkg/rpm tooling.
# `tauri build` runs beforeBuildCommand (npm run build) itself; esbuild's
# linux-x64 binary is static and runs in the SDK.
echo "==> [1/4] Building frontend + production binary inside the SDK ($SDK, glibc 2.42)"
flatpak run \
  --share=network \
  --filesystem="$ROOT" \
  --filesystem="$HOME/.cargo" \
  --env=PATH=/usr/lib/sdk/rust-stable/bin:/usr/lib/sdk/node20/bin:/usr/bin:/bin \
  --env=CARGO_HOME="$HOME/.cargo" \
  --env=CARGO_TARGET_DIR="$TARGET_DIR" \
  --command=bash "$SDK" -c "cd '$ROOT' && npm run tauri:build -- --no-bundle"

echo "==> [2/4] Preflight: assert binary needs no glibc newer than 2.$RUNTIME_GLIBC_MINOR"
max_glibc=$(objdump -T "$BIN" | grep -oE 'GLIBC_[0-9]+\.[0-9]+' | sort -uV | tail -1)
max_minor=${max_glibc##*.}
if (( max_minor > RUNTIME_GLIBC_MINOR )); then
  echo "ERROR: binary requires $max_glibc but the runtime only has 2.$RUNTIME_GLIBC_MINOR." >&2
  echo "       It would fail to start inside Flatpak. Did it build in the SDK?" >&2
  exit 1
fi
echo "    OK — highest required symbol is $max_glibc"

echo "==> [3/4] Assembling the Flatpak build tree (mirrors CI)"
mkdir -p "$BUILD_CACHE"
rm -rf "$BUILD_TREE" "$OSTREE_REPO"
flatpak build-init "$BUILD_TREE" "$APP_ID" "$SDK" "$PLATFORM"
mkdir -p \
  "$BUILD_TREE/files/bin" \
  "$BUILD_TREE/files/share/applications" \
  "$BUILD_TREE/files/share/icons/hicolor/128x128/apps" \
  "$BUILD_TREE/files/share/icons/hicolor/scalable/apps" \
  "$BUILD_TREE/files/share/metainfo" \
  "$BUILD_TREE/files/share/licenses/$APP_ID"
cp "$BIN"                                          "$BUILD_TREE/files/bin/clai"
cp "flatpak/$APP_ID.desktop"                       "$BUILD_TREE/files/share/applications/"
cp src-tauri/icons/128x128.png                     "$BUILD_TREE/files/share/icons/hicolor/128x128/apps/$APP_ID.png"
cp public/icon.svg                                 "$BUILD_TREE/files/share/icons/hicolor/scalable/apps/$APP_ID.svg"
cp "flatpak/$APP_ID.metainfo.xml"                  "$BUILD_TREE/files/share/metainfo/"
cp LICENSE                                         "$BUILD_TREE/files/share/licenses/$APP_ID/"

# --talk-name=org.freedesktop.Flatpak is REQUIRED: provider CLIs and the
# bash_exec sandbox run on the HOST via flatpak-spawn --host.
# --socket=x11 (NOT fallback-x11) is REQUIRED for image paste: arboard reads
# the image clipboard via the Wayland data-control protocol, which GNOME/Mutter
# does not implement, so it falls back to the X11 (XWayland) clipboard. With
# fallback-x11 the X11 socket is withheld whenever Wayland is present, leaving
# that fallback with no X server and silently breaking Ctrl+V image paste.
flatpak build-finish "$BUILD_TREE" \
  --command=clai \
  --share=ipc \
  --share=network \
  --socket=x11 \
  --socket=wayland \
  --device=dri \
  --filesystem=home \
  --talk-name=org.freedesktop.secrets \
  --talk-name=org.freedesktop.Flatpak

echo "==> [4/4] Exporting + bundling -> clai.flatpak"
flatpak build-export "$OSTREE_REPO" "$BUILD_TREE"
flatpak build-bundle "$OSTREE_REPO" clai.flatpak "$APP_ID"
echo "    Wrote $ROOT/clai.flatpak"

if [[ "${1:-}" == "--install" ]]; then
  echo "==> Installing (--user, reinstall to replace the running copy)"
  flatpak install --user --reinstall -y clai.flatpak
  echo "    Done. Run: flatpak run $APP_ID"
fi
