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
command -v node >/dev/null || { echo "node (22+) is required for the DevTools capture" >&2; exit 1; }
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
    # A run that was killed before its cleanup leaves the block behind; drop
    # it so it is never backed up or appended twice.
    if grep -q '^# temporary — scripts/web-screenshot.sh' .cargo/config.toml; then
        sed -i '' '/^# temporary — scripts\/web-screenshot.sh/,$d' .cargo/config.toml
        sed -i '' -e :a -e '/^\n*$/{$d;N;ba' -e '}' .cargo/config.toml
    fi
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

# Drive Chromium over DevTools (scripts/web-screenshot.mjs): navigate, let the
# wasm module boot and paint a few real frames, capture. Page console output
# lands in the log next to trunk's.
node "$ROOT/scripts/web-screenshot.mjs" "$CHROME" "http://127.0.0.1:$PORT/$QUERY" "$OUT" \
    "${PWRDE_WEB_SIZE:-1200x720}" "${PWRDE_WEB_SETTLE_MS:-6000}" "${PWRDE_WEB_KEYS:-}" "${PWRDE_WEB_CLICKS:-}" \
    "${PWRDE_WEB_DRAGS:-}" "${PWRDE_WEB_WHEEL:-}" 2>>"$ROOT/target/web-serve.log"
