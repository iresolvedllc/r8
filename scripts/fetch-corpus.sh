#!/usr/bin/env bash
# Fetches the pinned conformance corpora (see corpus-versions) under
# target/corpus/. Skips fetching when the checkout already matches the pin.
#
# Environment:
#   R8_TEST262_ROOT — use an existing test262 checkout instead of cloning
#                     (e.g. ../corpus/v8/test/test262/data once its submodule
#                     is checked out).
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
CORPUS_DIR="${R8_CORPUS_DIR:-$ROOT_DIR/target/corpus}"
VERSIONS="$ROOT_DIR/corpus-versions"

get_pin() {
    grep "^$1=" "$VERSIONS" | cut -d= -f2
}

# --- test262 ---------------------------------------------------------------
TEST262_REV="$(get_pin test262)"
TEST262_DIR="$CORPUS_DIR/test262"

if [ -n "${R8_TEST262_ROOT:-}" ]; then
    echo "corpus: using test262 from R8_TEST262_ROOT=$R8_TEST262_ROOT"
elif [ -d "$TEST262_DIR/test" ] && [ -d "$TEST262_DIR/harness" ]; then
    CURRENT="$(git -C "$TEST262_DIR" rev-parse HEAD 2>/dev/null || echo unknown)"
    if [ "$CURRENT" = "$TEST262_REV" ]; then
        echo "corpus: test262 already at $TEST262_REV"
    else
        echo "corpus: updating test262 $CURRENT -> $TEST262_REV"
        git -C "$TEST262_DIR" fetch --depth 1 origin "$TEST262_REV"
        git -C "$TEST262_DIR" checkout --force "$TEST262_REV"
    fi
else
    echo "corpus: cloning tc39/test262 at $TEST262_REV"
    rm -rf "$TEST262_DIR"
    git clone --quiet --filter=blob:none https://github.com/tc39/test262.git "$TEST262_DIR"
    git -C "$TEST262_DIR" checkout --quiet "$TEST262_REV"
fi

# --- wpt (reserved for the r8-wpt runner; not fetched yet) ------------------
# WPT_REV="$(get_pin wpt)"
