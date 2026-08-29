#!/usr/bin/env bash
# Build the wezterm fork branch that web/Cargo.toml patches the terminal crates
# onto: upstream wezterm at the rev pwrde's Cargo.lock pins, plus
# web/patches/wezterm-wasm.patch (four cfg fixes so the crates compile for
# wasm32). Clones into target/wezterm-fork; pass --push to force-push the
# branch to the fork remote.
#
#   scripts/web-wezterm-fork.sh          # prepare locally (used by the wasm check)
#   scripts/web-wezterm-fork.sh --push   # publish to $PWRDE_WEZTERM_FORK
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
REV=$(grep -A2 '^name = "wezterm-term"' "$ROOT/Cargo.lock" | sed -n 's/.*#\([0-9a-f]*\)".*/\1/p')
FORK=${PWRDE_WEZTERM_FORK:-git@github.com:a1re1/wezterm.git}
BRANCH=${PWRDE_WEZTERM_BRANCH:-pwrde-wasm-patches}
WORK=${PWRDE_WEZTERM_DIR:-$ROOT/target/wezterm-fork}

[ -n "$REV" ] || { echo "could not find the wezterm-term rev in Cargo.lock" >&2; exit 1; }

if [ ! -d "$WORK/.git" ]; then
    git clone --quiet --filter=blob:none https://github.com/wezterm/wezterm "$WORK"
fi
git -C "$WORK" fetch --quiet origin "$REV"
git -C "$WORK" checkout --quiet -B "$BRANCH" "$REV"
git -C "$WORK" apply --3way "$ROOT/web/patches/wezterm-wasm.patch"
git -C "$WORK" -c user.name=pwrde -c user.email=pwrde@localhost commit --quiet -am \
    "wasm32: compile filedescriptor/termwiz/escape-parser without file descriptors"
echo "$BRANCH = upstream $REV + web/patches/wezterm-wasm.patch, in $WORK"

# A cargo config that redirects the wezterm crates to this checkout, for
# building web/ before the branch is published (or to test a patch change):
#   cd web && cargo +nightly check --target wasm32-unknown-unknown \
#       --config ../target/wezterm-fork-patch.toml
PATCH_CFG="$ROOT/target/wezterm-fork-patch.toml"
{
    echo '[patch."https://github.com/wezterm/wezterm"]'
    for pair in termwiz:termwiz vtparse:vtparse wezterm-bidi:bidi \
        wezterm-blob-leases:wezterm-blob-leases wezterm-cell:wezterm-cell \
        wezterm-char-props:wezterm-char-props wezterm-color-types:color-types \
        wezterm-dynamic:wezterm-dynamic wezterm-dynamic-derive:wezterm-dynamic/derive \
        wezterm-escape-parser:wezterm-escape-parser wezterm-input-types:wezterm-input-types \
        wezterm-surface:wezterm-surface wezterm-term:term; do
        echo "${pair%%:*} = { path = \"$WORK/${pair#*:}\" }"
    done
} > "$PATCH_CFG"
echo "local patch config: $PATCH_CFG"

if [ "${1:-}" = "--push" ]; then
    git -C "$WORK" remote add fork "$FORK" 2>/dev/null || git -C "$WORK" remote set-url fork "$FORK"
    git -C "$WORK" push --force fork "$BRANCH"
    echo "pushed $BRANCH to $FORK"
fi
