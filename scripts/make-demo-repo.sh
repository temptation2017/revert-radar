#!/usr/bin/env bash
# Builds a throwaway git repo with a few reverts so you can try revert-radar.
# All data is made up. Usage: scripts/make-demo-repo.sh [dir]
set -euo pipefail
dir="${1:-demo-repo}"
rm -rf "$dir" && mkdir -p "$dir" && cd "$dir"
git init -q -b main
export GIT_AUTHOR_NAME="Demo Dev" GIT_AUTHOR_EMAIL="demo@example.com"
export GIT_COMMITTER_NAME="Demo Dev" GIT_COMMITTER_EMAIL="demo@example.com"

day=86400
start=$(( $(date +%s) - 200 * day ))
t=$start
commit() { # commit <offset-seconds> <file> <message>
  t=$(( t + $1 )); echo "$RANDOM" >> "$2"
  git add -A && GIT_AUTHOR_DATE="@$t" GIT_COMMITTER_DATE="@$t" git commit -qm "$3"
}
revert_last() { # revert_last <offset-seconds>
  t=$(( t + $1 ))
  GIT_AUTHOR_DATE="@$t" GIT_COMMITTER_DATE="@$t" git revert --no-edit HEAD >/dev/null
}

mkdir -p src/api src/ui docs
for i in $(seq 1 30); do commit $(( day * 2 )) "src/api/handler$((i % 4)).go" "api: tweak handler $i"; done
commit 3600 src/api/cache.go "feat: add response cache"; revert_last 1800
commit $day src/ui/theme.css "ui: new dark theme"
for i in $(seq 1 10); do commit $(( day * 2 )) "docs/guide$((i % 3)).md" "docs: update guide $i"; done
commit 7200 src/api/cache.go "feat: add response cache (take 2)"; revert_last $(( day * 3 ))
commit 600 src/api/auth.go "fix: token refresh race"; revert_last $(( day / 2 ))
revert_last 7200   # reland: revert the revert
for i in $(seq 1 12); do commit $day "src/ui/page$((i % 5)).vue" "ui: polish page $i"; done
commit 900 src/api/cache.go "perf: cache warmup on boot"; revert_last $(( day * 20 ))
commit 300 src/ui/theme.css "ui: tune contrast"; revert_last 420
echo "demo repo ready: $(pwd) ($(git rev-list --count HEAD) commits)"
