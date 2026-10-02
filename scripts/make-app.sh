#!/usr/bin/env bash
# Assemble Pwrde.app from built binaries.
#
# Usage: scripts/make-app.sh [release|debug]
# Produces: target/<profile>/Pwrde.app  (double-clickable macOS app bundle)
#
# Needs `cargo build --release` (or plain `cargo build` for `debug`) first:
# it bundles that profile's `pwrde` and `pwrde-helper` binaries.
#
# Web tabs run on Chromium (CEF, off-screen), which only works from a
# bundle, so this also lays out what CEF expects under Contents/Frameworks:
#
#   Chromium Embedded Framework.framework   copied from the CEF distribution
#                                           the `cef` crate's build downloaded
#                                           (target/<profile>/build/cef-dll-sys-*/out,
#                                           or $CEF_PATH when that is set)
#   Pwrde Helper.app                        the `pwrde-helper` binary, once per
#   Pwrde Helper (GPU).app                  process type Chromium launches
#   Pwrde Helper (Renderer).app
#   Pwrde Helper (Plugin).app
#   Pwrde Helper (Alerts).app
#
# `scripts/make-app.sh debug` is the dev path: a debug build has no web tabs
# when run as a bare binary (`cargo run` — opening one reports that Chromium
# is not bundled), so run target/debug/Pwrde.app/Contents/MacOS/pwrde instead.
#
# Signing is inside-out — framework, helpers, then the app — never `--deep`.
# The helpers get scripts/pwrde-helper.entitlements (JIT and unsigned
# executable memory for V8). By default everything is ad-hoc signed, which
# launches locally. For a Developer ID build set:
#
#   PWRDE_SIGN_IDENTITY     "Developer ID Application: Name (TEAMID)" — see
#                           `security find-identity -v -p codesigning`
#   PWRDE_TEAM_ID           the 10-character team id from that identity
#   PWRDE_PROVISION_PROFILE (optional) a .provisionprofile for ${BUNDLE_ID};
#                           when given it is embedded and the app is signed
#                           with scripts/pwrde.entitlements.in
#
# That profile and its com.apple.developer.web-browser.public-key-credential
# entitlement were what let the old WKWebView tabs use passkeys. Web tabs are
# Chromium now, so that WebKit path no longer applies and nothing here needs
# the profile; it is still honoured for a team that holds one. Never add the
# restricted entitlement to an ad-hoc build — macOS kills the process at launch.
set -euo pipefail

cd "$(dirname "$0")/.."

PROFILE_NAME="${1:-release}"
case "${PROFILE_NAME}" in
  release|debug) ;;
  *) echo "usage: scripts/make-app.sh [release|debug]" >&2; exit 64 ;;
esac

BIN_NAME="pwrde"
HELPER_BIN_NAME="pwrde-helper"
APP_NAME="Pwrde"
BUNDLE_ID="com.pwrde.terminal"
VERSION="$(grep -m1 '^version' Cargo.toml | sed -E 's/version *= *"([^"]+)".*/\1/')"

TARGET_DIR="target/${PROFILE_NAME}"
BIN_PATH="${TARGET_DIR}/${BIN_NAME}"
HELPER_BIN_PATH="${TARGET_DIR}/${HELPER_BIN_NAME}"
APP_DIR="${TARGET_DIR}/${APP_NAME}.app"
FRAMEWORKS_DIR="${APP_DIR}/Contents/Frameworks"
CEF_FRAMEWORK="Chromium Embedded Framework.framework"
HELPER_ENTITLEMENTS="scripts/pwrde-helper.entitlements"

BUILD_HINT="cargo build"
if [[ "${PROFILE_NAME}" == release ]]; then
  BUILD_HINT="cargo build --release"
fi
for bin in "${BIN_PATH}" "${HELPER_BIN_PATH}"; do
  if [[ ! -f "${bin}" ]]; then
    echo "error: ${bin} not found — run '${BUILD_HINT}' first" >&2
    exit 1
  fi
done

# The CEF distribution this build compiled against: $CEF_PATH when set (the
# same override the `cef` crate's build script honours), else the newest one
# the build script downloaded for this profile.
CEF_FRAMEWORK_SRC=""
if [[ -n "${CEF_PATH:-}" ]]; then
  CEF_FRAMEWORK_SRC="$(find "${CEF_PATH}" -maxdepth 3 -type d -name "${CEF_FRAMEWORK}" -print -quit 2>/dev/null || true)"
fi
if [[ -z "${CEF_FRAMEWORK_SRC}" ]]; then
  CEF_FRAMEWORK_SRC="$(ls -dt "${TARGET_DIR}"/build/cef-dll-sys-*/out/cef_macos_*/"${CEF_FRAMEWORK}" 2>/dev/null | head -1 || true)"
fi
if [[ -z "${CEF_FRAMEWORK_SRC}" || ! -d "${CEF_FRAMEWORK_SRC}" ]]; then
  echo "error: ${CEF_FRAMEWORK} not found under ${TARGET_DIR}/build (or \$CEF_PATH) — run '${BUILD_HINT}' first" >&2
  exit 1
fi

rm -rf "${APP_DIR}"
mkdir -p "${APP_DIR}/Contents/MacOS" "${APP_DIR}/Contents/Resources"

cp "${BIN_PATH}" "${APP_DIR}/Contents/MacOS/${BIN_NAME}"

# Chromium: the framework (cloned where the filesystem can, it is ~300 MB)
# and one helper bundle per process type around the same helper binary.
mkdir -p "${FRAMEWORKS_DIR}"
cp -Rc "${CEF_FRAMEWORK_SRC}" "${FRAMEWORKS_DIR}/" 2>/dev/null \
  || cp -R "${CEF_FRAMEWORK_SRC}" "${FRAMEWORKS_DIR}/"

# "<bundle name suffix>:<bundle id suffix>" — CEF derives each variant's path
# from the base helper's by appending the name suffix.
HELPERS=(":" " (GPU):.gpu" " (Renderer):.renderer" " (Plugin):.plugin" " (Alerts):.alerts")
for helper in "${HELPERS[@]}"; do
  HELPER_NAME="${APP_NAME} Helper${helper%%:*}"
  HELPER_APP="${FRAMEWORKS_DIR}/${HELPER_NAME}.app"
  mkdir -p "${HELPER_APP}/Contents/MacOS"
  cp "${HELPER_BIN_PATH}" "${HELPER_APP}/Contents/MacOS/${HELPER_NAME}"
  cat > "${HELPER_APP}/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleName</key>
	<string>${HELPER_NAME}</string>
	<key>CFBundleDisplayName</key>
	<string>${HELPER_NAME}</string>
	<key>CFBundleExecutable</key>
	<string>${HELPER_NAME}</string>
	<key>CFBundleIdentifier</key>
	<string>${BUNDLE_ID}.helper${helper#*:}</string>
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
	<!-- A background process: no Dock icon, no menu bar. -->
	<key>LSUIElement</key>
	<string>1</string>
	<key>LSEnvironment</key>
	<dict>
		<key>MallocNanoZone</key>
		<string>0</string>
	</dict>
	<key>NSSupportsAutomaticGraphicsSwitching</key>
	<true/>
</dict>
</plist>
PLIST
  echo "APPL????" > "${HELPER_APP}/Contents/PkgInfo"
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
	<key>NSSupportsAutomaticGraphicsSwitching</key>
	<true/>
	<!-- Chromium reaches these on a page's behalf (a passkey sign-in lists
	     paired Bluetooth devices); without a usage string macOS kills the
	     app outright instead of prompting. -->
	<key>NSBluetoothAlwaysUsageDescription</key>
	<string>Web pages in ${APP_NAME} can use Bluetooth security keys and devices.</string>
	<key>NSCameraUsageDescription</key>
	<string>Web pages in ${APP_NAME} can use the camera when you allow it.</string>
	<key>NSMicrophoneUsageDescription</key>
	<string>Web pages in ${APP_NAME} can use the microphone when you allow it.</string>
	<key>NSLocationUsageDescription</key>
	<string>Web pages in ${APP_NAME} can use your location when you allow it.</string>
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
			<!-- Web pages open as webview tabs; declaring the schemes lets the
			     app be chosen as a browser. Alternate rank keeps it from
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

echo "APPL????" > "${APP_DIR}/Contents/PkgInfo"

SIGN_IDENTITY="${PWRDE_SIGN_IDENTITY:-}"
TEAM_ID="${PWRDE_TEAM_ID:-}"
PROFILE="${PWRDE_PROVISION_PROFILE:-}"

# Sign inside-out: nested code first, the app last, so each outer seal covers
# already-final inner signatures. `sign <path> [extra codesign args…]`.
SIGN_ARGS=(--force --sign -)
sign() {
  local path="$1"
  shift
  codesign "${SIGN_ARGS[@]}" "$@" "${path}"
}
# Each step returns on failure explicitly: `set -e` is off while this runs as
# an `if` condition (the ad-hoc path), where only the last status would count.
sign_nested() {
  local lib
  while IFS= read -r lib; do
    sign "${lib}" || return 1
  done < <(find "${FRAMEWORKS_DIR}/${CEF_FRAMEWORK}/Libraries" -type f -name '*.dylib' 2>/dev/null)
  sign "${FRAMEWORKS_DIR}/${CEF_FRAMEWORK}" || return 1
  local helper
  for helper in "${FRAMEWORKS_DIR}"/*.app; do
    sign "${helper}" --entitlements "${HELPER_ENTITLEMENTS}" || return 1
  done
}

if [[ -n "${SIGN_IDENTITY}" || -n "${TEAM_ID}" || -n "${PROFILE}" ]]; then
  if [[ -z "${SIGN_IDENTITY}" || -z "${TEAM_ID}" ]]; then
    echo "error: PWRDE_SIGN_IDENTITY and PWRDE_TEAM_ID must both be set for a Developer ID build" >&2
    exit 1
  fi
  if [[ -n "${PROFILE}" && ! -f "${PROFILE}" ]]; then
    echo "error: provisioning profile not found: ${PROFILE}" >&2
    exit 1
  fi
  # Developer ID build: hardened runtime throughout, which is what makes the
  # helpers' JIT entitlements necessary.
  SIGN_ARGS=(--force --options runtime --timestamp --sign "${SIGN_IDENTITY}")
  sign_nested
  if [[ -n "${PROFILE}" ]]; then
    # The embedded profile authorizes the entitlements template's restricted
    # entries, whose identifiers must match it.
    cp "${PROFILE}" "${APP_DIR}/Contents/embedded.provisionprofile"
    ENT_DIR="$(mktemp -d)"
    trap 'rm -rf "${ENT_DIR}"' EXIT
    ENTITLEMENTS="${ENT_DIR}/pwrde.entitlements"
    sed -e "s/@TEAM_ID@/${TEAM_ID}/g" -e "s/@BUNDLE_ID@/${BUNDLE_ID}/g" \
      scripts/pwrde.entitlements.in > "${ENTITLEMENTS}"
    sign "${APP_DIR}" --entitlements "${ENTITLEMENTS}"
  else
    sign "${APP_DIR}"
  fi
  codesign --verify --deep --strict "${APP_DIR}"
  echo "signed ${APP_DIR} with ${SIGN_IDENTITY}"
else
  # Ad-hoc codesign so macOS will launch it locally (Gatekeeper still warns on
  # first open since it's unsigned by a Developer ID / unnotarized). The app
  # itself gets no entitlements here on purpose — see the header.
  if ! { sign_nested && sign "${APP_DIR}"; } >/dev/null 2>&1; then
    echo "warning: codesign failed (ad-hoc); app may need a right-click > Open" >&2
  fi
fi

echo "built ${APP_DIR} (v${VERSION})"
