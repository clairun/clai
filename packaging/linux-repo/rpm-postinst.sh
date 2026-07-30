#!/bin/sh
# %post for the clai .rpm — enrolls the CLAI rpm repository so future
# updates arrive through dnf / PackageKit / GNOME Software (the Chrome /
# VS Code model). The GPG key itself
# (/etc/pki/rpm-gpg/RPM-GPG-KEY-clai) is a regular packaged file; only
# the yum.repos.d entry is managed here, and rpm-postrm.sh removes it
# again on erase.
#
# The key is NOT imported into the rpm database here: running `rpm
# --import` from inside a transaction scriptlet contends for the rpmdb
# lock. dnf prompts once with the key fingerprint (read from the local
# gpgkey file) on the first repo-driven operation instead.
#
# Opt out: create /etc/sysconfig/clai containing CLAI_SKIP_RPM_REPO=1
# before installing/upgrading, and delete /etc/yum.repos.d/clai.repo.
# Upgrades then never re-add it.
#
# Scriptlet arg: $1 = 1 on fresh install, 2 on upgrade — enroll on both.
set -e

GPG_KEY=/etc/pki/rpm-gpg/RPM-GPG-KEY-clai
REPO_DIR=/etc/yum.repos.d
REPO_FILE=$REPO_DIR/clai.repo
DEFAULTS=/etc/sysconfig/clai

# The opt-out flag is grepped, not sourced: /etc/sysconfig/clai is
# root-owned either way, but grep keeps a stray `exit` or syntax error
# in it from breaking package installation under set -e.
if [ -r "$DEFAULTS" ] && grep -Eqs '^[[:space:]]*CLAI_SKIP_RPM_REPO[[:space:]]*=[[:space:]]*"?1"?[[:space:]]*$' "$DEFAULTS"; then
    exit 0
fi

# zypper-only systems (openSUSE) have no /etc/yum.repos.d; they enroll
# manually (`zypper ar`) as documented in packaging/linux-repo/README.md.
if [ -d "$REPO_DIR" ] && [ -r "$GPG_KEY" ]; then
    cat > "$REPO_FILE" <<EOF
[clai]
name=CLAI
baseurl=https://download.clai.run/rpm
enabled=1
type=rpm-md
gpgcheck=1
repo_gpgcheck=1
gpgkey=file://$GPG_KEY
metadata_expire=6h
EOF
fi

exit 0
