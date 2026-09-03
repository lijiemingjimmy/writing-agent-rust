#!/bin/sh
set -eu

repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd -P)
cd "$repo_root"

fail() {
  printf 'isolation check failed: %s\n' "$1" >&2
  exit 1
}

git_dir=$(git -C "$repo_root" rev-parse --absolute-git-dir)
common_relative=$(git -C "$repo_root" rev-parse --git-common-dir)
case "$common_relative" in
  /*) common_dir=$(CDPATH= cd -- "$common_relative" && pwd -P) ;;
  *) common_dir=$(CDPATH= cd -- "$repo_root/$common_relative" && pwd -P) ;;
esac
primary_root=$(git -C "$repo_root" worktree list --porcelain | awk 'index($0, "worktree ") == 1 { print substr($0, 10); exit }')
primary_root=$(CDPATH= cd -- "$primary_root" && pwd -P)
[ "$common_dir" = "$primary_root/.git" ] || fail "git common directory does not belong to the standalone Rust repository"
case "$git_dir" in
  "$common_dir"|"$common_dir"/worktrees/*) ;;
  *) fail "git directory escapes the standalone Rust repository" ;;
esac

bad_remote=$(git -C "$repo_root" remote -v | awk '{print $2}' | sort -u | grep -Ev '^(https://github\.com/lijiemingjimmy/writing-agent-rust(\.git)?|git@git\.tsinghua\.edu\.cn:lijm25/writing-agent-rust\.git|git@git\.tsinghua\.edu\.cn:rust-course/2026/agent/agent-lijm25\.git)$' || true)
[ -z "$bad_remote" ] || fail "unexpected Git remote: $bad_remote"

for forbidden_path in \
  app pyproject.toml .venv .github/workflows/deploy-pages.yml \
  web/src/teacher web/src/pages/TeacherDashboard.tsx web/dist-teacher; do
  [ ! -e "$repo_root/$forbidden_path" ] || fail "forbidden path exists: $forbidden_path"
done

database_file=$(find "$repo_root" -type f \
  \( -name '*.db' -o -name '*.db-wal' -o -name '*.db-shm' -o -name '*.db-journal' \
     -o -name '*.sqlite' -o -name '*.sqlite-wal' -o -name '*.sqlite-shm' -o -name '*.sqlite-journal' \) \
  -not -path '*/.git/*' -not -path '*/target/*' -not -path '*/node_modules/*' -print -quit)
[ -z "$database_file" ] || fail "database artifact exists: $database_file"

escaping_link=$(find "$repo_root" -type l -not -path '*/.git/*' -not -path '*/target/*' -not -path '*/node_modules/*' -print -quit)
[ -z "$escaping_link" ] || fail "symbolic link exists in deliverable: $escaping_link"

scan_paths="README.md rust-backend/README.md rust-backend/config.example.toml rust-backend/src web/src web/package.json web/vite.config.ts skills"
if rg -n 'VITE_TEACHER|/api/teacher|127\.0\.0\.1:8000|x-teacher-token|jiemingli\.top|lijiemingjimmy\.github\.io|/Users/lijieming|writing_coach\.db|语料/3\. 学生案例' $scan_paths >/dev/null; then
  rg -n 'VITE_TEACHER|/api/teacher|127\.0\.0\.1:8000|x-teacher-token|jiemingli\.top|lijiemingjimmy\.github\.io|/Users/lijieming|writing_coach\.db|语料/3\. 学生案例' $scan_paths >&2
  fail "runtime or frontend coupling marker found"
fi

tracked_link=$(git -C "$repo_root" ls-files -s | awk '$1 == "120000" { print $4; exit }')
[ -z "$tracked_link" ] || fail "tracked symbolic link exists: $tracked_link"

printf 'isolation check passed\n'
