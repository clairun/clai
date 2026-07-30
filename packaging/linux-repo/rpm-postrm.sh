#!/bin/sh
# %postun for the clai .rpm — removes the repository entry that
# rpm-postinst.sh created. The GPG key file is packaged, so rpm removes
# it by itself.
#
# Scriptlet arg: $1 = 0 on erase, 1 on upgrade — only clean up on erase,
# otherwise every upgrade would un-enroll the machine.
set -e

if [ "$1" = "0" ]; then
    rm -f /etc/yum.repos.d/clai.repo
fi

exit 0
