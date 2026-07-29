# Linux repositories (apt) and the shared signing key

CLAI publishes a GPG-signed **apt repository** to R2, served at
`https://download.clai.run/apt`. It is built by
`.github/workflows/apt.yml` after each Release (same trigger model as the
Flatpak repo: `workflow_run` on Release + manual `workflow_dispatch`
backfill from `main`).

## User-facing behavior

**Auto-enrollment (Chrome / VS Code model).** Every released `.deb` ships
the repo public key at `/usr/share/keyrings/clai-archive-keyring.gpg`
(a packaged file, see `bundle.linux.deb.files` in
`src-tauri/tauri.conf.json`) and its postinst (`deb-postinst.sh`) writes:

```
deb [arch=amd64 signed-by=/usr/share/keyrings/clai-archive-keyring.gpg] https://download.clai.run/apt stable main
```

to `/etc/apt/sources.list.d/clai.list`. From then on updates arrive
through `apt upgrade`, unattended-upgrades, and GNOME Software — even
when the app never runs. `deb-postrm.sh` removes the entry on
remove/purge.

**Opt-out:** create `/etc/default/clai` containing a `CLAI_SKIP_APT_REPO=1` line
(before install, or before the next upgrade) and delete
`/etc/apt/sources.list.d/clai.list`.

**Manual enrollment** (e.g. after installing a pre-repo `.deb`):

```bash
sudo curl -fsSLo /usr/share/keyrings/clai-archive-keyring.gpg \
  https://download.clai.run/apt/clai-archive-keyring.gpg
echo "deb [arch=amd64 signed-by=/usr/share/keyrings/clai-archive-keyring.gpg] https://download.clai.run/apt stable main" | \
  sudo tee /etc/apt/sources.list.d/clai.list
sudo apt update && sudo apt install clai
```

## Repo layout on R2 (bucket `clai-releases`, prefix `apt/`)

```
apt/clai-archive-keyring.gpg      # binary keyring (served for manual enrollment)
apt/clai-repo-pubkey.asc          # same key, ASCII-armored
apt/dists/stable/Release          # apt-ftparchive metadata
apt/dists/stable/InRelease        # clearsigned Release
apt/dists/stable/Release.gpg      # detached signature
apt/dists/stable/main/binary-amd64/Packages{,.gz}
apt/pool/main/c/clai/clai_<ver>_amd64.deb   # newest 10 versions retained
```

A flat `apt-ftparchive`-generated tree — no reprepro/aptly database to
persist. The pool is pulled from R2, the new `.deb` added, metadata
regenerated over everything, and pushed back (pool first, then dists, so
metadata never references an object that is not uploaded yet). Retention
keeps the 10 newest versions; older ones remain on the GitHub Releases
page.

## The shared signing key (maintainer)

One key signs all CLAI Linux repos — the apt repo and the Flatpak OSTree
repo (`flatpak/README.md`), and the future rpm repo:

- **uid:** `CLAI Package Repo <packages@clai.run>`
- **fingerprint:** `CDA8 29F9 CE92 F153 43B5 CC77 9850 A80F 5EA6 7D26`
- **private half:** exists ONLY as the `LINUX_REPO_GPG_PRIVATE_KEY`
  GitHub Actions secret (ASCII-armored, **no passphrase** — CI must use
  it non-interactively) plus the maintainer's offline backup.
- **public half:** committed here as `clai-repo-pubkey.asc` /
  `clai-archive-keyring.gpg` (the dearmored binary form that ships in
  the `.deb` and is served from R2).

Generated with:

```bash
export GNUPGHOME=$(mktemp -d)
gpg --batch --pinentry-mode loopback --passphrase '' \
  --quick-generate-key "CLAI Package Repo <packages@clai.run>" rsa4096 sign never
gpg --armor --export-secret-keys packages@clai.run   # -> LINUX_REPO_GPG_PRIVATE_KEY secret
gpg --armor --export packages@clai.run > clai-repo-pubkey.asc
gpg --dearmor < clai-repo-pubkey.asc > clai-archive-keyring.gpg
```

(`--pinentry-mode loopback --passphrase ''` matters: without it gpg still
routes a passphrase prompt through pinentry — headless it errors, on a
desktop it pops a dialog — and a passphrase-protected key would import
fine in CI but then fail at signing time.)

`apt.yml` refuses to publish if the secret's fingerprint does not match
the committed public key: enrolled machines verify against the keyring
inside their installed `.deb`, so metadata signed by any other key would
break `apt update` on every enrolled machine.

**If the key leaks**, an attacker who can also write to the R2 bucket
can serve malicious packages: rotate by generating a new key, replacing
the secret AND the two committed public files, shipping a release whose
`.deb` carries the new keyring, and re-running the apt + Flatpak
workflows. Already-enrolled apt machines keep working once they upgrade
to a `.deb` carrying the new keyring (install it manually from the
Releases page if `apt update` already fails); Flatpak remotes must be
re-added from the fresh `.flatpakrepo`.

**If the key is lost**, the same rotation applies — this is why the
maintainer keeps an offline backup of the private key.

**Tradeoff (deliberate):** sharing one key across apt/Flatpak/rpm means
one compromise affects all Linux channels, in exchange for a single
secret and one rotation story. Acceptable at CLAI's scale; revisit if
the project grows dedicated infra.
