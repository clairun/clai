# Linux repositories (apt, rpm) and the shared signing key

CLAI publishes GPG-signed **apt** and **rpm repositories** to R2, served
at `https://download.clai.run/apt` and `https://download.clai.run/rpm`.
They are built by `.github/workflows/apt.yml` and
`.github/workflows/rpm.yml` after each Release (same trigger model as the
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

## rpm: user-facing behavior

**Auto-enrollment.** Every released `.rpm` ships the repo public key at
`/etc/pki/rpm-gpg/RPM-GPG-KEY-clai` (a packaged file, see
`bundle.linux.rpm.files` in `src-tauri/tauri.conf.json`) and its `%post`
(`rpm-postinst.sh`) writes `/etc/yum.repos.d/clai.repo` pointing at
`https://download.clai.run/rpm` with `gpgcheck=1 repo_gpgcheck=1` and the
local key file. From then on updates arrive through `dnf upgrade`,
PackageKit, and GNOME Software. `rpm-postrm.sh` (`%postun`) removes the
entry on erase only (`$1 = 0`), never on upgrade.

The key is deliberately NOT `rpm --import`ed from the scriptlet (rpmdb
lock contention inside a transaction); dnf prompts once with the
fingerprint from the local key file instead.

**Opt-out:** create `/etc/sysconfig/clai` containing a
`CLAI_SKIP_RPM_REPO=1` line (before install, or before the next upgrade)
and delete `/etc/yum.repos.d/clai.repo`.

**Manual enrollment** (Fedora/RHEL — e.g. after installing a pre-repo
`.rpm`):

```bash
sudo curl -fsSLo /etc/yum.repos.d/clai.repo https://download.clai.run/rpm/clai.repo
sudo dnf install clai
```

openSUSE (no `/etc/yum.repos.d`, so `%post` skips auto-enrollment there):

```bash
sudo zypper ar -f https://download.clai.run/rpm clai
sudo zypper install clai
```

## rpm: repo layout on R2 (bucket `clai-releases`, prefix `rpm/`)

```
rpm/clai.repo                     # dnf/zypper repo file (manual enrollment)
rpm/clai-repo-pubkey.asc          # ASCII-armored public key
rpm/repodata/repomd.xml           # createrepo_c metadata
rpm/repodata/repomd.xml.asc       # detached signature (repo_gpgcheck=1)
rpm/repodata/repomd.xml.key       # key copy at zypper's expected name
rpm/packages/clai-<ver>-1.x86_64.rpm   # newest 10 versions retained
```

Same push-pull model as apt: packages pulled from R2, the new `.rpm`
added and **individually signed** (`rpmsign --addsign`, so `gpgcheck=1`
holds), metadata regenerated with `createrepo_c`, `repomd.xml` signed,
and pushed back (packages first, then repodata). The workflow verifies
the embedded package signature against the committed public key in an
isolated rpmdb before publishing anything.

## The shared signing key (maintainer)

One key signs all CLAI Linux repos — the apt repo and the Flatpak OSTree
repo (`flatpak/README.md`), and the rpm repo:

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

`apt.yml` and `rpm.yml` refuse to publish if the secret's fingerprint
does not match the committed public key: enrolled machines verify against the keyring
inside their installed `.deb`, so metadata signed by any other key would
break `apt update` on every enrolled machine.

**If the key leaks**, an attacker who can also write to the R2 bucket
can serve malicious packages: rotate by generating a new key, replacing
the secret AND the two committed public files, shipping a release whose
`.deb` carries the new keyring, and re-running the apt + rpm + Flatpak
workflows. For rpm, also delete the retained `rpm/packages/*` objects
from R2 first: their embedded signatures were made with the old key, so
`gpgcheck=1` clients could no longer install them — the re-run
re-populates the pool with the latest release, freshly signed. Already-enrolled apt machines keep working once they upgrade
to a `.deb` carrying the new keyring (install it manually from the
Releases page if `apt update` already fails); Flatpak remotes must be
re-added from the fresh `.flatpakrepo`.

**If the key is lost**, the same rotation applies — this is why the
maintainer keeps an offline backup of the private key.

**Tradeoff (deliberate):** sharing one key across apt/Flatpak/rpm means
one compromise affects all Linux channels, in exchange for a single
secret and one rotation story. Acceptable at CLAI's scale; revisit if
the project grows dedicated infra.
