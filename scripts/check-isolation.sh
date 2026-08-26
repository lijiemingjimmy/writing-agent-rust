#!/bin/sh
set -eu

repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd -P)
cd "$repo_root"

fail() {
  printf 'isolation check failed: %s\n' "$1" >&2
  exit 1
}

[ -d "$repo_root/.git" ] || fail ".git must be a directory owned by this repository"

git_dir=$(git -C "$repo_root" rev-parse --absolute-git-dir)
common_relative=$(git -C "$repo_root" rev-parse --git-common-dir)
common_dir=$(CDPATH= cd -- "$repo_root/$common_relative" && pwd -P)
[ "$git_dir" = "$repo_root/.git" ] || fail "git directory escapes the repository"
[ "$common_dir" = "$repo_root/.git" ] || fail "git common directory is shared"

worktree_count=$(git -C "$repo_root" worktree list --porcelain | awk '$1 == "worktree" { count += 1 } END { print count + 0 }')
[ "$worktree_count" -eq 1 ] || fail "expected exactly one worktree"

bad_remote=$(git -C "$repo_root" remote -v | awk '{print $2}' | sort -u | grep -Ev '^(https://github\.com/lijiemingjimmy/writing-agent-rust(\.git)?|https://git\.tsinghua\.edu\.cn/.+)$' || true)
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
