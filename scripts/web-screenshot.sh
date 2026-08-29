#!/usr/bin/env bash
# Build the wasm page, serve it with the COOP/COEP headers gpui_web needs, and
# screenshot it with headless Chromium — the visual-testing loop that a native
# macOS window can't offer to browser automation.
#
#   scripts/web-screenshot.sh [out.png] [query]
#
#   out.png  where to write (default target/web-screenshot.png)
#   query    appended to the URL, e.g. '?backend=webgl'
#
# Env: PWRDE_CHROME (headless Chromium binary; defaults to Playwright's
# chrome-headless-shell), PWRDE_WEB_PORT (8090), PWRDE_WEB_SIZE (1200x720),
# PWRDE_WEB_SETTLE_MS (real time to let the wasm boot and paint, 6000),
# PWRDE_WEB_CLICKS ("x,y;x,y" clicked after the settle), PWRDE_WEB_DRAGS
# ("x1,y1>x2,y2" press-move-release), PWRDE_WEB_WHEEL ("x,y,dy"), PWRDE_WEB_KEYS
# (text typed last; \n = Enter, \b = Backspace, \M-p = ⌘P),
# PWRDE_WEB_RELEASE=1 for an optimized build, PWRDE_WEZTERM_LOCAL=1 to build
# against the local wezterm checkout from scripts/web-wezterm-fork.sh instead
# of the published fork branch (see scripts/web-serve.sh).
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
OUT=${1:-$ROOT/target/web-screenshot.png}
QUERY=${2:-}
PORT=${PWRDE_WEB_PORT:-8090}
CHROME=${PWRDE_CHROME:-$(ls -d "$HOME"/Library/Caches/ms-playwright/chromium_headless_shell-*/chrome-headless-shell-mac-arm64/chrome-headless-shell 2>/dev/null | tail -1 || true)}

[ -x "${CHROME:-}" ] || {
    echo "no headless Chromium found; set PWRDE_CHROME or run: npx playwright install chromium" >&2
    exit 1
}
command -v node >/dev/null || { echo "node (22+) is required for the DevTools capture" >&2; exit 1; }
mkdir -p "$(dirname "$OUT")" "$ROOT/target"

SERVE=""
cleanup() {
    if [ -n "$SERVE" ]; then
        kill "$SERVE" 2>/dev/null || true
        wait "$SERVE" 2>/dev/null || true
    fi
}
trap cleanup EXIT

# scripts/web-serve.sh builds and serves (and, with PWRDE_WEZTERM_LOCAL=1,
# swaps the wezterm patch to the local checkout for the run). Wait for trunk
# to announce the server — a probe alone could hit a previous run's dying
# server and capture a page that is still being rebuilt.
: >"$ROOT/target/web-serve.log"
PWRDE_WEB_PORT="$PORT" "$ROOT/scripts/web-serve.sh" ${PWRDE_WEB_RELEASE:+--release} \
    >"$ROOT/target/web-serve.log" 2>&1 &
SERVE=$!
for _ in $(seq 1 6000); do
    if ! kill -0 "$SERVE" 2>/dev/null; then
        echo "web-serve exited; see target/web-serve.log" >&2
        tail -20 "$ROOT/target/web-serve.log" >&2
        exit 1
    fi
    grep -q 'server listening' "$ROOT/target/web-serve.log" && break
    sleep 0.2
done

# Drive Chromium over DevTools (scripts/web-screenshot.mjs): navigate, let the
# wasm module boot and paint a few real frames, capture. Page console output
# lands in the log next to trunk's.
node "$ROOT/scripts/web-screenshot.mjs" "$CHROME" "http://127.0.0.1:$PORT/$QUERY" "$OUT" \
    "${PWRDE_WEB_SIZE:-1200x720}" "${PWRDE_WEB_SETTLE_MS:-6000}" "${PWRDE_WEB_KEYS:-}" "${PWRDE_WEB_CLICKS:-}" \
    "${PWRDE_WEB_DRAGS:-}" "${PWRDE_WEB_WHEEL:-}" 2>>"$ROOT/target/web-serve.log"
