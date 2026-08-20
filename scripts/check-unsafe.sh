#!/usr/bin/env bash
# Reflex unsafe-code policy check (P0.1: "deny unsafe by default").
#
# Scans every `src/` directory in the workspace for the token `unsafe`
# and fails unless each occurrence is explicitly allowlisted in
# `unsafe-allowlist.txt`. This is the enforceable mechanism behind
# `#![forbid(unsafe_code)]` until it is wired into `cargo xtask check`
# (see the repository governance report, WS-0b).
#
# Usage: scripts/check-unsafe.sh
# Exit:  0 if every `unsafe` occurrence is allowlisted, 1 otherwise.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ALLOWLIST="$ROOT/unsafe-allowlist.txt"

# Lines that match the allowlist entry format: "<path>:<line>: ..."
normalize_allowlist() {
    grep -E '^[^#[:space:]]' "$ALLOWLIST" 2>/dev/null || true
}

allowlist_entries() {
    normalize_allowlist | awk -F: '{print $1 ":" $2}'
}

violations=0

while IFS= read -r file; do
    line_no=0
    while IFS= read -r line; do
        line_no=$((line_no + 1))
        case "$line" in
            *unsafe*) ;;
            *) continue ;;
        esac
        # Skip prose/comments and identifier-like tokens (unsafe-allowlist, unsafe_check).
        case "$line" in
            *'//'*unsafe* | *'#'*unsafe*) continue ;;
            *'unsafe-'* | *'unsafe_'* | *'forbid(unsafe'* | *'"unsafe-'* | *"'unsafe-"*) continue ;;
            *'check-unsafe'* | *'unsafe_check'*) continue ;;
        esac
        hit="${file#./}:$line_no"
        if ! allowlist_entries | grep -qxF "$hit"; then
            echo "UNSAFE NOT ALLOWLISTED: $hit"
            echo "  add to $ALLOWLIST as: $hit: <owner> — <rationale> — <expiry>"
            violations=$((violations + 1))
        fi
    done <"$file"
done < <(find "$ROOT" -type d \( -name target -o -name .git -o -name node_modules \) -prune -o -type f -name '*.rs' -print | sed "s|$ROOT/||")

if [ "$violations" -gt 0 ]; then
    echo "unsafe-code policy FAILED: $violations unallowlisted occurrence(s)"
    exit 1
fi

echo "unsafe-code policy OK: no unallowlisted 'unsafe' in src/ trees"