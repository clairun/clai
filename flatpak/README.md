# Flatpak

The previous local Flatpak packaging flow copied a host-built Linux binary into
the Flatpak. That is not reliable. If the host glibc is newer than the runtime
glibc, the app fails at launch with errors like `GLIBC_2.43 not found`.

Use the manifest at the repo root to build the app inside the Flatpak SDK:

```bash
flatpak run org.flatpak.Builder \
  --user \
  --install-deps-from=flathub \
  --force-clean \
  --install \
  --repo=repo \
  .flatpak-builder/build \
  io.github.juacker.clai.yml

flatpak run io.github.juacker.clai
```

To create a bundle after the build:

```bash
flatpak build-bundle repo clai.flatpak io.github.juacker.clai
```

Notes:

- This manifest is suitable for local and CI builds.
- It is not fully Flathub-ready yet.
- Flathub builds do not allow network access during the build, so npm and cargo
  dependencies must be supplied through generated dependency manifests.
- The current sandbox permissions are intentionally broad for local execution
  features and may need review before Flathub submission.
