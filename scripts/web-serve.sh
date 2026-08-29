#!/usr/bin/env bash
# Serve the wasm32 page with trunk (http://127.0.0.1:$PWRDE_WEB_PORT, default
# 8090). With PWRDE_WEZTERM_LOCAL=1 the wezterm crates come from the checkout
# scripts/web-wezterm-fork.sh prepares (target/wezterm-fork) instead of the
# published fork branch: web/Cargo.toml's `[patch]` entries are rewritten to
# `path = …` for the run and restored when this script exits, however it
# exits. Extra arguments go to `trunk serve` (e.g. --release).
#
#   scripts/web-serve.sh                       # published fork branch
#   PWRDE_WEZTERM_LOCAL=1 scripts/web-serve.sh  # local checkout
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
PORT=${PWRDE_WEB_PORT:-8090}
MANIFEST="$ROOT/web/Cargo.toml"
BACKUP=""
CHILD=""

cleanup() {
    if [ -n "$CHILD" ]; then
        kill "$CHILD" 2>/dev/null || true
        wait "$CHILD" 2>/dev/null || true
    fi
    if [ -n "$BACKUP" ] && [ -f "$BACKUP" ]; then
        mv "$BACKUP" "$MANIFEST"
    fi
}
trap cleanup EXIT
trap 'exit 143' TERM INT

if [ "${PWRDE_WEZTERM_LOCAL:-}" = 1 ]; then
    PATCH_CFG="$ROOT/target/wezterm-fork-patch.toml"
    [ -f "$PATCH_CFG" ] || "$ROOT/scripts/web-wezterm-fork.sh" >/dev/null
    # A previous run killed before its cleanup leaves the swapped manifest
    # behind; the committed one is the reference.
    if grep -q 'wezterm-fork' "$MANIFEST"; then
        git -C "$ROOT" checkout -- web/Cargo.toml
    fi
    BACKUP="$ROOT/target/web-Cargo.toml.bak"
    cp "$MANIFEST" "$BACKUP"
    # Replace the `[patch."…wezterm"]` section body (up to the next blank
    # line) with the path entries the fork script generated.
    python3 - "$MANIFEST" "$PATCH_CFG" <<'EOF'
import sys
manifest, patch = sys.argv[1], sys.argv[2]
text = open(manifest).read()
header = '[patch."https://github.com/wezterm/wezterm"]\n'
start = text.index(header) + len(header)
end = text.index("\n\n", start)
entries = "".join(l for l in open(patch).read().splitlines(True) if not l.startswith("["))
open(manifest, "w").write(text[:start] + entries.rstrip("\n") + text[end:])
EOF
fi

cd "$ROOT/web"
trunk serve --no-autoreload --port "$PORT" "$@" &
CHILD=$!
wait "$CHILD"
