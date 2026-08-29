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
# chrome-headless-shell), PWRDE_WEB_PORT (8090), PWRDE_WEB_SIZE (1200,720),
# PWRDE_WEB_BUDGET_MS (virtual time to let the wasm boot and paint, 15000),
# PWRDE_WEB_RELEASE=1 for an optimized build, PWRDE_WEZTERM_LOCAL=1 to build
# against the local wezterm checkout from scripts/web-wezterm-fork.sh instead
# of the published fork branch (trunk cannot pass cargo `--config`, so the
# patch table is appended to web/.cargo/config.toml for the run and restored).
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
mkdir -p "$(dirname "$OUT")" "$ROOT/target"

cd "$ROOT/web"
SERVE=""
CONFIG_BAK=""
cleanup() {
    [ -n "$SERVE" ] && kill "$SERVE" 2>/dev/null || true
    [ -n "$CONFIG_BAK" ] && cp "$CONFIG_BAK" .cargo/config.toml
}
trap cleanup EXIT

if [ "${PWRDE_WEZTERM_LOCAL:-}" = 1 ]; then
    PATCH_CFG="$ROOT/target/wezterm-fork-patch.toml"
    [ -f "$PATCH_CFG" ] || "$ROOT/scripts/web-wezterm-fork.sh" >/dev/null
    CONFIG_BAK="$ROOT/target/web-cargo-config.bak"
    cp .cargo/config.toml "$CONFIG_BAK"
    { echo; echo "# temporary — scripts/web-screenshot.sh (PWRDE_WEZTERM_LOCAL) restores this file"; cat "$PATCH_CFG"; } >> .cargo/config.toml
fi

# `trunk serve` builds first, then listens; the loop below waits for it.
trunk serve --no-autoreload --port "$PORT" ${PWRDE_WEB_RELEASE:+--release} \
    >"$ROOT/target/web-serve.log" 2>&1 &
SERVE=$!
for _ in $(seq 1 3000); do
    if ! kill -0 $SERVE 2>/dev/null; then
        echo "trunk serve exited; see target/web-serve.log" >&2
        tail -20 "$ROOT/target/web-serve.log" >&2
        exit 1
    fi
    curl -fs "http://127.0.0.1:$PORT/" >/dev/null && break
    sleep 0.2
done

# SwiftShader gives headless Chromium a software WebGL/WebGPU device; the
# virtual time budget lets the wasm module boot and paint before the capture.
"$CHROME" \
    --headless=new --no-sandbox --hide-scrollbars \
    --window-size="${PWRDE_WEB_SIZE:-1200,720}" \
    --use-angle=swiftshader --enable-unsafe-swiftshader --enable-unsafe-webgpu \
    --virtual-time-budget="${PWRDE_WEB_BUDGET_MS:-15000}" \
    --screenshot="$OUT" \
    "http://127.0.0.1:$PORT/$QUERY" 2>>"$ROOT/target/web-serve.log"
echo "$OUT"
