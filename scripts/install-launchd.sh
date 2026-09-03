#!/bin/sh
set -eu

usage() {
  echo "usage: DRY_RUN=1 $0 --label LABEL --binary ABS_PATH --config ABS_PATH --workdir ABS_PATH --log-dir ABS_PATH" >&2
  exit 2
}

label=""
binary=""
config=""
workdir=""
log_dir=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    --label) [ "$#" -ge 2 ] || usage; label=$2; shift 2 ;;
    --binary) [ "$#" -ge 2 ] || usage; binary=$2; shift 2 ;;
    --config) [ "$#" -ge 2 ] || usage; config=$2; shift 2 ;;
    --workdir) [ "$#" -ge 2 ] || usage; workdir=$2; shift 2 ;;
    --log-dir) [ "$#" -ge 2 ] || usage; log_dir=$2; shift 2 ;;
    *) usage ;;
  esac
done

case "$label" in ''|*[!A-Za-z0-9._-]*) usage ;; esac
for path in "$binary" "$config" "$workdir" "$log_dir"; do
  case "$path" in /*) ;; *) echo "all paths must be absolute" >&2; exit 2 ;; esac
  case "$path" in *'&'*|*'<'*|*'>'*|*'|'*|*'\'*) echo "paths contain unsupported characters" >&2; exit 2 ;; esac
done

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
template="$script_dir/../deploy/macos/wam-rust-agent.plist.template"
target="$HOME/Library/LaunchAgents/$label.plist"
stdout_path="$log_dir/stdout.log"
stderr_path="$log_dir/stderr.log"

if [ "${DRY_RUN:-0}" = "1" ]; then
  echo "DRY_RUN: would render $template to $target"
  echo "DRY_RUN: binary=$binary config=$config workdir=$workdir logs=$log_dir"
  exit 0
fi

[ -x "$binary" ] || { echo "binary is not executable: $binary" >&2; exit 1; }
[ -r "$config" ] || { echo "config is not readable: $config" >&2; exit 1; }
[ -d "$workdir" ] || { echo "working directory does not exist: $workdir" >&2; exit 1; }
[ -r "$template" ] || { echo "plist template is missing" >&2; exit 1; }

mkdir -p "$log_dir" "$HOME/Library/LaunchAgents"
temporary=$(mktemp "${TMPDIR:-/tmp}/wam-launchd.XXXXXX")
trap 'rm -f "$temporary"' EXIT HUP INT TERM
sed \
  -e "s|__LABEL__|$label|g" \
  -e "s|__BINARY__|$binary|g" \
  -e "s|__CONFIG__|$config|g" \
  -e "s|__WORKDIR__|$workdir|g" \
  -e "s|__STDOUT__|$stdout_path|g" \
  -e "s|__STDERR__|$stderr_path|g" \
  "$template" > "$temporary"
plutil -lint "$temporary" >/dev/null
if [ -e "$target" ]; then
  cp "$target" "$target.backup.$(date +%Y%m%d%H%M%S)"
  launchctl bootout "gui/$(id -u)" "$target" 2>/dev/null || true
fi
install -m 600 "$temporary" "$target"
launchctl bootstrap "gui/$(id -u)" "$target"
launchctl kickstart -k "gui/$(id -u)/$label"
echo "installed and started $label"
