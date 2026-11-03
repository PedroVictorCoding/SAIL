#!/usr/bin/env bash
# Prepare the SAIL data disk (/srv/sail-data) so SAIL and its owner can use it without root.
#
#   sudo scripts/setup-sail-data.sh --user <service-user> [--mount /srv/sail-data] [--dry-run]
#
# What it does:
# - checks the path is a mounted filesystem (an unmounted mount point is on the root SSD);
# - creates the `sail-data` group and adds the service user to it;
# - makes the root and SAIL's directories owned by <service-user>:sail-data, mode 2770
#   (setgid: new directories keep the group; no access for other users);
# - gives existing data to the group (g+rwX, o-rwx) and, when setfacl exists, a default ACL
#   so new files stay group-accessible;
# - writes /etc/tmpfiles.d/sail-data.conf so the layout is re-checked at boot.
#
# It refuses to add the sandbox runner user (`sail-runner`) to the group: sandboxed code
# must never be able to write SAIL's own data. Hot paths (worktrees, Cargo target, active
# models) stay on the runtime SSD (/srv/sail) and are not touched.
set -euo pipefail

MOUNT=/srv/sail-data
GROUP=sail-data
RUNNER_USER=sail-runner
SERVICE_USER=""
DRY_RUN=0
ALLOW_UNMOUNTED=0
SUBDIRS=(state forge arena guardian logs)

usage() {
  sed -n '2,20p' "$0" | sed 's/^# \{0,1\}//'
  echo "Options: --user USER  --mount PATH  --allow-unmounted  --dry-run"
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --user) SERVICE_USER="${2:?}"; shift 2 ;;
    --mount) MOUNT="${2:?}"; shift 2 ;;
    --allow-unmounted) ALLOW_UNMOUNTED=1; shift ;;
    --dry-run) DRY_RUN=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) echo "unknown option: $1" >&2; usage >&2; exit 2 ;;
  esac
done

run() {
  if [[ $DRY_RUN == 1 ]]; then
    printf 'would run:'; printf ' %q' "$@"; printf '\n'
  else
    "$@"
  fi
}

die() { echo "error: $*" >&2; exit 1; }

[[ -n $SERVICE_USER ]] || die "--user is required (the account that runs sail-core)"
[[ $DRY_RUN == 1 || $EUID -eq 0 ]] || die "run with sudo (or use --dry-run)"
id "$SERVICE_USER" >/dev/null 2>&1 || die "user $SERVICE_USER does not exist"
[[ $SERVICE_USER != "$RUNNER_USER" ]] || die "the sandbox runner user cannot own SAIL data"
[[ $SERVICE_USER != root ]] || die "SAIL should not run as root; pass the service user"
[[ -d $MOUNT ]] || die "$MOUNT does not exist"
if ! mountpoint -q "$MOUNT"; then
  [[ $ALLOW_UNMOUNTED == 1 ]] || die "$MOUNT is not a mounted filesystem. Mount the data disk first (or pass --allow-unmounted on a machine without one)"
  echo "warning: $MOUNT is not a mount point; continuing because of --allow-unmounted" >&2
fi

if id "$RUNNER_USER" >/dev/null 2>&1 && id -nG "$RUNNER_USER" | tr ' ' '\n' | grep -qx "$GROUP"; then
  die "$RUNNER_USER is a member of $GROUP; remove it (gpasswd -d $RUNNER_USER $GROUP) before continuing"
fi

if ! getent group "$GROUP" >/dev/null; then
  run groupadd --system "$GROUP"
fi
if ! id -nG "$SERVICE_USER" | tr ' ' '\n' | grep -qx "$GROUP"; then
  run usermod -aG "$GROUP" "$SERVICE_USER"
fi

for dir in "$MOUNT" "${SUBDIRS[@]/#/$MOUNT/}"; do
  [[ -L $dir ]] && die "$dir is a symlink; refusing to change ownership through it"
  run install -d -o "$SERVICE_USER" -g "$GROUP" -m 2770 "$dir"
done

# Existing data: group-owned and group-accessible, nothing for others. Symlinks are not
# followed or changed: chgrp -h touches a link itself, and find only visits regular files
# and directories.
run chgrp -R -h "$GROUP" "$MOUNT"
run find "$MOUNT" -xdev \( -type d -exec chmod g+rwxs,o-rwx {} + \) -o \( -type f -exec chmod g+rwX,o-rwx {} + \)
if command -v setfacl >/dev/null; then
  run setfacl -R -d -m "g:$GROUP:rwX" -m "o::---" "$MOUNT"
else
  echo "note: setfacl not found (apt install acl); new files follow the creating process's umask" >&2
fi

TMPFILES=/etc/tmpfiles.d/sail-data.conf
CONF="# Written by SAIL scripts/setup-sail-data.sh
d $MOUNT 2770 $SERVICE_USER $GROUP -"
for sub in "${SUBDIRS[@]}"; do
  CONF+=$'\n'"d $MOUNT/$sub 2770 $SERVICE_USER $GROUP -"
done
if [[ $DRY_RUN == 1 ]]; then
  echo "would write $TMPFILES:"; echo "$CONF"
else
  printf '%s\n' "$CONF" > "$TMPFILES"
fi

echo
echo "SAIL data root: $MOUNT (owner $SERVICE_USER, group $GROUP, mode 2770)"
echo "Members of $GROUP can read and manage SAIL data without root."
echo "Log out and back in (or run 'newgrp $GROUP') for $SERVICE_USER's new group to apply."
