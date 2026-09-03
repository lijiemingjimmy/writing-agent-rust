#!/bin/sh
set -eu

health_url="http://127.0.0.1:3000/health"
config=""
binary=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    --health-url) [ "$#" -ge 2 ] || exit 2; health_url=$2; shift 2 ;;
    --config) [ "$#" -ge 2 ] || exit 2; config=$2; shift 2 ;;
    --binary) [ "$#" -ge 2 ] || exit 2; binary=$2; shift 2 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

case "$health_url" in http://127.0.0.1:*'/health'|http://localhost:*'/health') ;; *) echo "health URL must be a loopback /health endpoint" >&2; exit 2 ;; esac
[ -z "$config" ] || [ -r "$config" ] || { echo "config is not readable: $config" >&2; exit 1; }
[ -z "$binary" ] || [ -x "$binary" ] || { echo "binary is not executable: $binary" >&2; exit 1; }
response=$(curl --fail --silent --show-error --max-time 5 "$health_url")
case "$response" in *'"status":"ok"'*'"service":"writing-coach-rust"'*) ;; *) echo "unexpected health response" >&2; exit 1 ;; esac
echo "Rust service health check passed: $health_url"
