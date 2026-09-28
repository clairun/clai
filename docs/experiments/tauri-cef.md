# Tauri CEF runtime experiment (Linux)

This branch is a disposable UI performance comparison. It is not intended for merging into `main`.

## Versions and wiring

- Tauri `3.0.0-alpha.3`, `tauri-build 3.0.0-alpha.2`, `tauri-runtime-cef 3.0.0-alpha.4`, and Rust 1.95.0. All five Rust plugins use `3.0.0-alpha.1`; the JavaScript API/plugins and CLI use their matching published v3 alpha versions.
- CEF runtime selection is `tauri::Builder::default().runtime(tauri_runtime_cef::Cef::default())`. The `#[tauri_runtime_cef::cef_entry_point]` on `main` dispatches Chromium helper processes. The current checkout contains no explicit `Wry` types; Tauri's default builder uses a dynamic runtime.
- `tauri-runtime-cef` depends on Tauri v3 and GTK4. It cannot be added to this app's Tauri v2 dependency graph without migrating Tauri and plugins. No plugin was disabled, and no app Rust code changed beyond the two lines above. Tauri v3 and `tauri-runtime-cef` declare `rust-version = "1.95"`.
- `vendor/tauri-plugin-dialog` is an unmodified copy of the published `tauri-plugin-dialog 3.0.0-alpha.1` crate with one change: `src/desktop.rs` imports `tauri::Manager`. The published crate calls `AppHandle::run_on_main_thread`, which Tauri 3 moved onto the `Manager` trait, so it fails with `E0599`. As of 2026-09-28, `3.0.0-alpha.1` is still the newest release on crates.io. `[patch.crates-io]` in `src-tauri/Cargo.toml` points at the copy. Drop both once a fixed alpha ships.
- `tauri-plugin-dialog` is built with `default-features = false, features = ["xdg-portal"]`. Its default `gtk3` feature makes `rfd` link `libgtk-3`. With GTK3 and GTK4 in one process, `gtk4::init()`'s `gtk_init_check()` resolves to GTK3's symbol. GTK4 then reports itself uninitialised, and the first launch panicked with `CreateWindow(... "GTK was not actually initialized")`. With `xdg-portal`, `ldd target/debug/clai` lists only `libgtk-4.so.1`. File dialogs go through the XDG desktop portal (a portal backend such as `xdg-desktop-portal-gtk`/`-gnome`/`-kde` must be running). Message/ask dialogs fall back to `zenity`, which must be installed.

## Build and run on Linux

Install Rust >= 1.95 (the system `cargo` 1.90 is too old), Node/npm, GTK4 development libraries, and X11 development libraries. CEF's Linux integration uses X11; a Wayland desktop needs XWayland. On Debian or Ubuntu, start with `libgtk-4-dev`, `libx11-dev`, `libxkbcommon-dev`, `pkg-config`, `build-essential`, and `cmake`. The host used here reports GTK 4.22.5 through `pkg-config`.

```bash
rustup toolchain install 1.95.0 --profile minimal
npm ci
RUSTUP_TOOLCHAIN=1.95.0 npm run tauri dev
```

To check/build the Rust crate directly:

```bash
cd src-tauri
cargo +1.95.0 check
cargo +1.95.0 build
```

The first Cargo build downloads CEF via `cef-dll-sys` / `download-cef` from `cef-builds.spotifycdn.com` into Cargo's ignored build output by default. This revision pins CEF `152.3.0+152.0.6`, whose Linux minimal archive is named `cef_binary_152.0.6+g708dc14+chromium-152.0.7977.83_linux64_minimal.tar.bz2`. The archive observed here is 321,503,907 bytes (about 307 MiB); extraction and build output require substantially more space. `CEF_PATH` can point at an existing distribution or a writable download directory. Upstream's example describes the full distribution as roughly 1 GB.

## Status and caveats

- **Build: OK.** `cargo build` (debug) succeeded with Rust 1.95.0 on 2026-09-28. A cold build took about 11 minutes with 4 jobs. The debug binary is about 570 MiB and `libcef.so` about 1.4 GiB. The build copies the CEF runtime (`libcef.so`, `*.pak`, `icudtl.dat`, `locales/`, `v8_context_snapshot.bin`) next to the binary in `target/debug/`.
- **Launch: not verified.** The task sandbox has no `DISPLAY`/`WAYLAND_DISPLAY` and no Xvfb. Running `target/debug/clai` reaches CEF initialisation and then panics with `CreateWindow(... "Failed to initialize GTK")`, which is expected without a display. The first real launch has to happen on the owner's desktop.
- The app uses the same identifier (`run.clai.CLAI`) and data directory (`~/.config/clai`) as the production build. The Rust code and DB schema are unchanged, but back up `~/.config/clai` before testing anyway. CEF keeps its profile in `~/.cache/run.clai.CLAI/cef`.
- Disk usage: `src-tauri/target/debug/build/cef-dll-sys-*/out` holds about 1.8 GiB (archive plus extracted distribution).
- Untested and likely rough areas: native dialogs (XDG portal + zenity), clipboard, opener, the updater (disable auto-update while testing), bundling (`tauri build` was not attempted), and the xterm WebGL addon under CEF GPU flags.
- CEF/Tauri v3 is alpha software. Runtime and plugin behaviour, updater bundling, permissions, and parity with the current WebKitGTK app need hands-on testing. The Linux CEF feature uses X11, and the upstream runtime warns that some webview attributes are unsupported.
- To compare against the WebKitGTK build, check out `main` in a separate worktree and run it the same way. This branch is not meant to be merged.

Upstream references: [CEF example](https://github.com/tauri-apps/tauri/tree/tauri-runtime-cef-v3.0.0-alpha.4/examples/cef), [runtime changelog](https://github.com/tauri-apps/tauri/blob/tauri-runtime-cef-v3.0.0-alpha.4/crates/tauri-runtime-cef/CHANGELOG.md), [runtime manifest](https://github.com/tauri-apps/tauri/blob/tauri-runtime-cef-v3.0.0-alpha.4/crates/tauri-runtime-cef/Cargo.toml).
