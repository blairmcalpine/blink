#!/bin/sh
# stop: run `just lint` (host + Windows clippy). On failure, send the tail of the
# output back as a follow-up so the agent keeps fixing. Never blocks: missing
# tools, a non-completed stop, or the loop limit all end quietly.

max_loops=3
max_lines=150
max_bytes=12000

input=$(cat)
command -v jq >/dev/null 2>&1 || exit 0

status=$(printf '%s' "$input" | jq -r '.status // "completed"' 2>/dev/null) || exit 0
loop_count=$(printf '%s' "$input" | jq -r '.loop_count // 0' 2>/dev/null) || exit 0

[ "$status" = "completed" ] || exit 0
case "$loop_count" in
    '' | *[!0-9]*) loop_count=0 ;;
esac
[ "$loop_count" -lt "$max_loops" ] || exit 0

# shellcheck source=/dev/null
[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"
command -v cargo >/dev/null 2>&1 || exit 0
command -v just >/dev/null 2>&1 || exit 0

cd "$(dirname "$0")/../.." 2>/dev/null || exit 0

if output=$(CARGO_TERM_COLOR=never just lint 2>&1); then
    exit 0
fi

tail_output=$(printf '%s\n' "$output" | tail -n "$max_lines" | tail -c "$max_bytes")

jq -n --arg out "$tail_output" '{
  followup_message: ("`just lint` failed. Fix every error below without weakening lints, `clippy.toml`, or the toolchain, and without `#[allow]` (use `#[expect(lint, reason = \"...\")]` only when the lint is genuinely wrong). Then rerun `just lint`.\n\n```\n" + $out + "\n```")
}'
exit 0
