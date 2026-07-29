#!/bin/sh
# postinst for the clai .deb — enrolls the CLAI apt repository so future
# updates arrive through apt / unattended-upgrades / GNOME Software
# (the Chrome / VS Code model). The keyring itself
# (/usr/share/keyrings/clai-archive-keyring.gpg) is a regular packaged
# file; only the sources.list.d entry is managed here, and deb-postrm.sh
# removes it again on remove/purge.
#
# Opt out: create /etc/default/clai containing CLAI_SKIP_APT_REPO=1
# before installing/upgrading, and delete
# /etc/apt/sources.list.d/clai.list. Upgrades then never re-add it.
set -e

KEYRING=/usr/share/keyrings/clai-archive-keyring.gpg
SOURCES=/etc/apt/sources.list.d/clai.list
DEFAULTS=/etc/default/clai

case "$1" in
    configure)
        # The opt-out flag is grepped, not sourced: /etc/default/clai is
        # root-owned either way, but grep keeps a stray `exit` or syntax
        # error in it from breaking package configuration under set -e.
        if [ -r "$DEFAULTS" ] && grep -Eqs '^[[:space:]]*CLAI_SKIP_APT_REPO[[:space:]]*=[[:space:]]*"?1"?[[:space:]]*$' "$DEFAULTS"; then
            exit 0
        fi
        if [ -r "$KEYRING" ]; then
            echo "deb [arch=amd64 signed-by=$KEYRING] https://download.clai.run/apt stable main" > "$SOURCES"
        fi
        ;;
esac

exit 0
