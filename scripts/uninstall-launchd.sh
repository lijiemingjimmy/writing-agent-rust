#!/bin/sh
set -eu

if [ "$#" -ne 2 ] || [ "$1" != "--label" ]; then
  echo "usage: DRY_RUN=1 $0 --label LABEL" >&2
  exit 2
fi
label=$2
case "$label" in ''|*[!A-Za-z0-9._-]*) echo "invalid label" >&2; exit 2 ;; esac
target="$HOME/Library/LaunchAgents/$label.plist"

if [ "${DRY_RUN:-0}" = "1" ]; then
  echo "DRY_RUN: would stop $label and move $target to Trash"
  exit 0
fi

if [ ! -e "$target" ]; then
  echo "service is not installed: $label"
  exit 0
fi
launchctl bootout "gui/$(id -u)" "$target" 2>/dev/null || true
trash_dir="$HOME/.Trash"
mkdir -p "$trash_dir"
destination="$trash_dir/$label.plist.$(date +%Y%m%d%H%M%S)"
mv "$target" "$destination"
echo "stopped $label; plist moved to $destination"
