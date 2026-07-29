#!/bin/sh
# postrm for the clai .deb — removes the apt repository entry that
# deb-postinst.sh created. The keyring file is packaged, so dpkg removes
# it by itself.
set -e

case "$1" in
    remove|purge)
        rm -f /etc/apt/sources.list.d/clai.list
        ;;
esac

exit 0
