#!/bin/sh
# afterFileEdit: rustfmt the edited file if it is Rust. Never blocks the agent.

input=$(cat)

command -v jq >/dev/null 2>&1 || exit 0
file=$(printf '%s' "$input" | jq -r '.file_path // empty' 2>/dev/null) || exit 0

case "$file" in
    *.rs) ;;
    *) exit 0 ;;
esac
[ -f "$file" ] || exit 0

# shellcheck source=/dev/null
[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"
command -v rustfmt >/dev/null 2>&1 || exit 0

# rustfmt picks up rustfmt.toml (edition included) from the file's ancestors.
cd "$(dirname "$0")/../.." 2>/dev/null || exit 0
rustfmt "$file" >/dev/null 2>&1 || true
exit 0
