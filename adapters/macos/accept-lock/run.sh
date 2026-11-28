#!/bin/bash
# Break-it lock for the accept check. Opens throwaway TextEdit instances (your own
# TextEdit windows and settings are untouched), runs the cases with macOS
# autocorrect OFF (deterministic, simulated host rewrites) and ON (real,
# best-effort), then closes them. Needs Accessibility for this Terminal.
#
#   ./run.sh             test the shipped guard (must pass)
#   ./run.sh --sabotage  remove the length check (must FAIL)
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
work="$(mktemp -d)"
pid=""
cleanup() { [ -n "$pid" ] && kill "$pid" 2>/dev/null; rm -rf "$work"; }
trap cleanup EXIT

cp "$here/../Sources/typeassist-input-macos/AcceptGuard.swift" "$work/"
if [ "${1:-}" = "--sabotage" ]; then
    # Trust the engine's delete count again, as before the fix.
    sed -i '' 's|        if onScreen == expected {|        if true \|\| onScreen == expected {|' "$work/AcceptGuard.swift"
    grep -q 'if true ||' "$work/AcceptGuard.swift" || { echo "sabotage did not apply"; exit 2; }
fi
swiftc -O "$work/AcceptGuard.swift" "$here/main.swift" -o "$work/lock" 2>&1 | grep -E 'error' && exit 2

status=0
for mode in off on; do
    flag=$([ $mode = on ] && echo YES || echo NO)
    : > "$work/accept-lock.txt"
    before="$(pgrep -x TextEdit || true)"
    open -n -g -a TextEdit "$work/accept-lock.txt" --args -ApplePersistenceIgnoreState YES \
        -CorrectSpellingAutomatically $flag -CheckSpellingWhileTyping $flag -SmartQuotes NO
    sleep 3
    pid="$(comm -13 <(echo "$before" | sort) <(pgrep -x TextEdit | sort) | head -1)"
    [ -n "$pid" ] || { echo "could not open a throwaway TextEdit"; exit 2; }
    "$work/lock" "$pid" $mode || status=1
    kill "$pid"; pid=""
    sleep 1
done
[ $status = 0 ] && echo "ACCEPT LOCK: GREEN" || echo "ACCEPT LOCK: RED"
exit $status
