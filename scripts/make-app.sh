#!/usr/bin/env bash
# Assemble Pwrde.app from the release binary.
#
# Usage: scripts/make-app.sh
# Produces: target/release/Pwrde.app  (double-clickable macOS app bundle)
#
# Webview tabs are Chromium (CEF): the bundle carries
# Contents/Frameworks/Chromium Embedded Framework.framework and the five
# helper apps Chromium launches its subprocesses from (Pwrde Helper.app and
# its " (GPU)" / " (Renderer)" / " (Plugin)" / " (Alerts)" siblings, each a
# copy of target/release/pwrde-helper). The framework comes from the CEF
# binary distribution the build used: $CEF_PATH (cef-rs's versioned layout or
# a flat export-cef-dir export), else the copy cef-dll-sys downloaded into
# target/release/build.
#
# Signing. Everything is signed inside-out — the framework's libraries, the
# framework, each helper, then the app. By default the bundle is ad-hoc
# signed, which launches locally but carries no entitlements. To produce a
# Developer ID build that carries the web-browser passkey entitlement
# (Touch ID / iCloud Keychain), set all three:
#
#   PWRDE_SIGN_IDENTITY     "Developer ID Application: Name (TEAMID)" — see
#                           `security find-identity -v -p codesigning`
#   PWRDE_TEAM_ID           the 10-character team id from that identity
#   PWRDE_PROVISION_PROFILE path to a .provisionprofile for ${BUNDLE_ID} that
#                           carries com.apple.developer.web-browser.public-key-credential
#
# The entitlement is restricted: Apple grants it per team (Account Holder
# request at https://developer.apple.com/contact/request/macos-browsers-passkeys/),
# after which the profile is downloadable from the developer portal. Do not
# add it to an ad-hoc build — macOS kills the process at launch.
set -euo pipefail

cd "$(dirname "$0")/.."

BIN_NAME="pwrde"
APP_NAME="Pwrde"
BUNDLE_ID="com.pwrde.terminal"
VERSION="$(grep -m1 '^version' Cargo.toml | sed -E 's/version *= *"([^"]+)".*/\1/')"

BIN_PATH="target/release/${BIN_NAME}"
APP_DIR="target/release/${APP_NAME}.app"

# CEF: the subprocess helper binary, the helper bundles' base name (Chromium
# derives the suffixed siblings from it — keep in step with HELPER_NAME /
# HELPER_SUFFIXES in src/cef_app.rs), and the framework.
HELPER_BIN_PATH="target/release/pwrde-helper"
HELPER_NAME="${APP_NAME} Helper"
HELPER_SUFFIXES=("" " (GPU)" " (Renderer)" " (Plugin)" " (Alerts)")
CEF_FRAMEWORK="Chromium Embedded Framework.framework"

if [[ ! -f "${BIN_PATH}" || ! -f "${HELPER_BIN_PATH}" ]]; then
  echo "error: ${BIN_PATH} or ${HELPER_BIN_PATH} not found — run 'cargo build --release' first" >&2
  exit 1
fi

# The CEF distribution matching the cef crate in Cargo.lock ("154.3.0+154.0.32"
# -> CEF 154.0.32). Candidates, first hit wins: cef-rs's versioned layout under
# $CEF_PATH, $CEF_PATH itself, then the build script's own download.
CEF_VERSION="$(awk '/^name = "cef-dll-sys"$/ { getline; gsub(/.*\+|"/, ""); print; exit }' Cargo.lock)"
CEF_ARCH="$(uname -m)"
[[ "${CEF_ARCH}" == "arm64" ]] && CEF_ARCH="aarch64"
CEF_DIR=""
for candidate in \
  "${CEF_PATH:+${CEF_PATH}/${CEF_VERSION}/cef_macos_${CEF_ARCH}}" \
  "${CEF_PATH:-}" \
  target/release/build/cef-dll-sys-*/out/cef_macos_"${CEF_ARCH}"; do
  if [[ -n "${candidate}" && -d "${candidate}/${CEF_FRAMEWORK}" ]]; then
    CEF_DIR="${candidate}"
    break
  fi
done
if [[ -z "${CEF_DIR}" ]]; then
  echo "error: no ${CEF_FRAMEWORK} for CEF ${CEF_VERSION} — set CEF_PATH to the directory the build used" >&2
  exit 1
fi
# Only the versioned candidate is the pinned build by construction; a flat
# $CEF_PATH or a stale build dir can hold any CEF, so check what was found.
CEF_FOUND="$(/usr/libexec/PlistBuddy -c "Print :CFBundleShortVersionString" \
  "${CEF_DIR}/${CEF_FRAMEWORK}/Resources/Info.plist" 2>/dev/null || true)"
if [[ "${CEF_FOUND}" != "${CEF_VERSION}" && "${CEF_FOUND}" != "${CEF_VERSION}."* ]]; then
  echo "error: ${CEF_DIR} holds CEF ${CEF_FOUND:-<unknown>}, but Cargo.lock pins ${CEF_VERSION}" >&2
  exit 1
fi

rm -rf "${APP_DIR}"
mkdir -p "${APP_DIR}/Contents/MacOS" "${APP_DIR}/Contents/Resources" "${APP_DIR}/Contents/Frameworks"

cp "${BIN_PATH}" "${APP_DIR}/Contents/MacOS/${BIN_NAME}"

# Chromium: the framework, then one helper bundle per process flavour.
FRAMEWORKS_DIR="${APP_DIR}/Contents/Frameworks"
cp -R "${CEF_DIR}/${CEF_FRAMEWORK}" "${FRAMEWORKS_DIR}/${CEF_FRAMEWORK}"

for suffix in "${HELPER_SUFFIXES[@]}"; do
  helper="${HELPER_NAME}${suffix}"
  helper_app="${FRAMEWORKS_DIR}/${helper}.app"
  # com.pwrde.terminal.helper, .helper.gpu, .helper.renderer, …
  flavour="$(printf '%s' "${suffix}" | tr -cd '[:alpha:]' | tr '[:upper:]' '[:lower:]')"
  helper_id="${BUNDLE_ID}.helper${flavour:+.${flavour}}"
  mkdir -p "${helper_app}/Contents/MacOS"
  cp "${HELPER_BIN_PATH}" "${helper_app}/Contents/MacOS/${helper}"
  cat > "${helper_app}/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleName</key>
	<string>${helper}</string>
	<key>CFBundleDisplayName</key>
	<string>${helper}</string>
	<key>CFBundleExecutable</key>
	<string>${helper}</string>
	<key>CFBundleIdentifier</key>
	<string>${helper_id}</string>
	<key>CFBundlePackageType</key>
	<string>APPL</string>
	<key>CFBundleInfoDictionaryVersion</key>
	<string>6.0</string>
	<key>CFBundleShortVersionString</key>
	<string>${VERSION}</string>
	<key>CFBundleVersion</key>
	<string>${VERSION}</string>
	<key>LSMinimumSystemVersion</key>
	<string>11.0</string>
	<!-- Helpers never show in the Dock. -->
	<key>LSUIElement</key>
	<string>1</string>
	<key>NSSupportsAutomaticGraphicsSwitching</key>
	<true/>
</dict>
</plist>
PLIST
  echo "APPL????" > "${helper_app}/Contents/PkgInfo"
done

# Generate AppIcon.icns from the source PNG (2048x2048 recommended).
ICON_SRC="resources/pwrde-app-icon.png"
if [[ -f "${ICON_SRC}" ]]; then
  ICONSET="$(mktemp -d)/AppIcon.iconset"
  mkdir -p "${ICONSET}"
  for size in 16 32 128 256 512; do
    sips -z "${size}" "${size}" "${ICON_SRC}" --out "${ICONSET}/icon_${size}x${size}.png" >/dev/null
    sips -z "$((size * 2))" "$((size * 2))" "${ICON_SRC}" --out "${ICONSET}/icon_${size}x${size}@2x.png" >/dev/null
  done
  iconutil -c icns "${ICONSET}" -o "${APP_DIR}/Contents/Resources/AppIcon.icns"
  rm -rf "$(dirname "${ICONSET}")"
else
  echo "warning: ${ICON_SRC} not found; bundling without an app icon" >&2
fi

cat > "${APP_DIR}/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleName</key>
	<string>${APP_NAME}</string>
	<key>CFBundleDisplayName</key>
	<string>${APP_NAME}</string>
	<key>CFBundleExecutable</key>
	<string>${BIN_NAME}</string>
	<key>CFBundleIdentifier</key>
	<string>${BUNDLE_ID}</string>
	<key>CFBundleIconFile</key>
	<string>AppIcon</string>
	<key>CFBundlePackageType</key>
	<string>APPL</string>
	<key>CFBundleInfoDictionaryVersion</key>
	<string>6.0</string>
	<key>CFBundleShortVersionString</key>
	<string>${VERSION}</string>
	<key>CFBundleVersion</key>
	<string>${VERSION}</string>
	<key>LSMinimumSystemVersion</key>
	<string>11.0</string>
	<key>NSHighResolutionCapable</key>
	<true/>
	<key>LSApplicationCategoryType</key>
	<string>public.app-category.developer-tools</string>
	<key>CFBundleDocumentTypes</key>
	<array>
		<dict>
			<key>CFBundleTypeName</key>
			<string>Folder</string>
			<key>CFBundleTypeRole</key>
			<string>Viewer</string>
			<key>LSHandlerRank</key>
			<string>Alternate</string>
			<key>LSItemContentTypes</key>
			<array>
				<string>public.folder</string>
			</array>
		</dict>
	</array>
	<key>CFBundleURLTypes</key>
	<array>
		<dict>
			<key>CFBundleURLName</key>
			<string>${BUNDLE_ID}</string>
			<key>CFBundleTypeRole</key>
			<string>Viewer</string>
			<key>CFBundleURLSchemes</key>
			<array>
				<string>pwrde</string>
			</array>
		</dict>
		<dict>
			<!-- Web pages open as webview tabs; declaring the schemes is one of
			     Apple's criteria for the passkey entitlement above and lets
			     the app be chosen as a browser. Alternate rank keeps it from
			     claiming links by default. -->
			<key>CFBundleURLName</key>
			<string>Web page</string>
			<key>CFBundleTypeRole</key>
			<string>Viewer</string>
			<key>LSHandlerRank</key>
			<string>Alternate</string>
			<key>CFBundleURLSchemes</key>
			<array>
				<string>http</string>
				<string>https</string>
			</array>
		</dict>
	</array>
</dict>
</plist>
PLIST
# The privacy usage strings web tabs need (Bluetooth for passkeys, camera,
# microphone): macOS aborts the app, rather than denying, when one is missing.
/usr/libexec/PlistBuddy -c "Merge scripts/privacy-usage.plist" "${APP_DIR}/Contents/Info.plist"

echo "APPL????" > "${APP_DIR}/Contents/PkgInfo"

# Sign the nested Chromium code, innermost first: the framework's own
# libraries, the framework, then each helper app. "$@" is the codesign
# options for the framework; helpers add HELPER_SIGN_ARGS (their entitlements
# in a Developer ID build). The outer app is signed by the caller, last.
HELPER_SIGN_ARGS=()
sign_nested() {
  local framework="${FRAMEWORKS_DIR}/${CEF_FRAMEWORK}"
  local lib suffix
  for lib in "${framework}/Libraries/"*.dylib; do
    [[ -f "${lib}" ]] && codesign --force "$@" "${lib}"
  done
  codesign --force "$@" "${framework}"
  for suffix in "${HELPER_SUFFIXES[@]}"; do
    codesign --force "$@" ${HELPER_SIGN_ARGS[@]+"${HELPER_SIGN_ARGS[@]}"} \
      "${FRAMEWORKS_DIR}/${HELPER_NAME}${suffix}.app"
  done
}

SIGN_IDENTITY="${PWRDE_SIGN_IDENTITY:-}"
TEAM_ID="${PWRDE_TEAM_ID:-}"
PROFILE="${PWRDE_PROVISION_PROFILE:-}"

if [[ -n "${SIGN_IDENTITY}" || -n "${TEAM_ID}" || -n "${PROFILE}" ]]; then
  if [[ -z "${SIGN_IDENTITY}" || -z "${TEAM_ID}" || -z "${PROFILE}" ]]; then
    echo "error: PWRDE_SIGN_IDENTITY, PWRDE_TEAM_ID and PWRDE_PROVISION_PROFILE must all be set for a Developer ID build" >&2
    exit 1
  fi
  if [[ ! -f "${PROFILE}" ]]; then
    echo "error: provisioning profile not found: ${PROFILE}" >&2
    exit 1
  fi
  # Developer ID build: the embedded profile authorizes the restricted
  # passkey entitlement, and the entitlements' identifiers must match it.
  cp "${PROFILE}" "${APP_DIR}/Contents/embedded.provisionprofile"
  ENT_DIR="$(mktemp -d)"
  trap 'rm -rf "${ENT_DIR}"' EXIT
  ENTITLEMENTS="${ENT_DIR}/pwrde.entitlements"
  sed -e "s/@TEAM_ID@/${TEAM_ID}/g" -e "s/@BUNDLE_ID@/${BUNDLE_ID}/g" \
    scripts/pwrde.entitlements.in > "${ENTITLEMENTS}"
  # Under the hardened runtime Chromium's helpers need to JIT (V8, in the
  # renderer) and map unsigned executable memory; these are the entitlements
  # CEF's own signing guide gives every helper. They are not restricted, so
  # no profile is involved.
  HELPER_ENTITLEMENTS="${ENT_DIR}/pwrde-helper.entitlements"
  cat > "${HELPER_ENTITLEMENTS}" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>com.apple.security.cs.allow-jit</key>
	<true/>
	<key>com.apple.security.cs.allow-unsigned-executable-memory</key>
	<true/>
	<key>com.apple.security.cs.disable-library-validation</key>
	<true/>
</dict>
</plist>
PLIST
  HELPER_SIGN_ARGS=(--entitlements "${HELPER_ENTITLEMENTS}")
  sign_nested --options runtime --timestamp --sign "${SIGN_IDENTITY}"
  codesign --force --options runtime --timestamp \
    --sign "${SIGN_IDENTITY}" --entitlements "${ENTITLEMENTS}" "${APP_DIR}"
  codesign --verify --deep --strict "${APP_DIR}"
  echo "signed ${APP_DIR} with ${SIGN_IDENTITY} (passkey entitlement embedded)"
else
  # Ad-hoc codesign so macOS will launch it locally (Gatekeeper still warns on
  # first open since it's unsigned by a Developer ID / unnotarized). No
  # entitlements here on purpose — see the header. The nested Chromium code
  # must sign (an unsigned arm64 helper is killed at launch), so that part
  # fails the script; only the outer app's signature stays best-effort.
  sign_nested --sign -
  codesign --force --sign - "${APP_DIR}" >/dev/null 2>&1 || \
    echo "warning: codesign failed (ad-hoc); app may need a right-click > Open" >&2
fi

echo "built ${APP_DIR} (v${VERSION})"
