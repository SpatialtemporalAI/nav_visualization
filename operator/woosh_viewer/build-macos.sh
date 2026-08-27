#!/bin/bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"
DIST_DIR="${SCRIPT_DIR}/dist"
DEPLOYMENT_TARGET="${MACOSX_DEPLOYMENT_TARGET:-12.0}"
ARM64_BINARY="${SCRIPT_DIR}/target/aarch64-apple-darwin/release/woosh-viewer"
X86_64_BINARY="${SCRIPT_DIR}/target/x86_64-apple-darwin/release/woosh-viewer"
PACKAGE_ONLY=0

while [[ $# -gt 0 ]]; do
    case "$1" in
        --package-only)
            PACKAGE_ONLY=1
            shift
            ;;
        --arm64-binary)
            ARM64_BINARY="$2"
            shift 2
            ;;
        --x86_64-binary)
            X86_64_BINARY="$2"
            shift 2
            ;;
        *)
            echo "Unknown argument: $1" >&2
            exit 2
            ;;
    esac
done

if [[ "$(uname -s)" != "Darwin" ]]; then
    echo "This script must run on macOS because Apple SDK tools are required." >&2
    exit 1
fi

export MACOSX_DEPLOYMENT_TARGET="${DEPLOYMENT_TARGET}"

if [[ ${PACKAGE_ONLY} -eq 0 ]]; then
    rustup target add aarch64-apple-darwin x86_64-apple-darwin
    (
        cd "${SCRIPT_DIR}"
        cargo build --release --locked --target aarch64-apple-darwin
        cargo build --release --locked --target x86_64-apple-darwin
    )
fi

for binary in "${ARM64_BINARY}" "${X86_64_BINARY}"; do
    if [[ ! -f "${binary}" ]]; then
        echo "Missing macOS binary: ${binary}" >&2
        exit 1
    fi
done

mkdir -p "${DIST_DIR}" "${SCRIPT_DIR}/target"
STAGE_DIR="$(mktemp -d "${SCRIPT_DIR}/target/macos-package.XXXXXX")"
trap 'rm -rf "${STAGE_DIR}"' EXIT

APP_PATH="${STAGE_DIR}/Woosh Viewer.app"
CONTENTS_DIR="${APP_PATH}/Contents"
MACOS_DIR="${CONTENTS_DIR}/MacOS"
RESOURCES_DIR="${CONTENTS_DIR}/Resources"
mkdir -p "${MACOS_DIR}" "${RESOURCES_DIR}"

lipo -create "${ARM64_BINARY}" "${X86_64_BINARY}" -output "${MACOS_DIR}/woosh-viewer"
chmod 755 "${MACOS_DIR}/woosh-viewer"
cp "${SCRIPT_DIR}/macos/Info.plist" "${CONTENTS_DIR}/Info.plist"
cp "${SCRIPT_DIR}/README-MACOS.md" "${RESOURCES_DIR}/README-MACOS.md"
cp "${REPO_ROOT}/LICENSE-MIT" "${RESOURCES_DIR}/LICENSE-MIT"
cp "${REPO_ROOT}/LICENSE-APACHE" "${RESOURCES_DIR}/LICENSE-APACHE"
cp "${SCRIPT_DIR}/woosh-viewer.example.toml" "${RESOURCES_DIR}/woosh-viewer.example.toml"

SIGN_IDENTITY="${MACOS_CODESIGN_IDENTITY:--}"
codesign --force --deep --sign "${SIGN_IDENTITY}" --timestamp=none "${APP_PATH}"
codesign --verify --deep --strict "${APP_PATH}"
lipo -info "${MACOS_DIR}/woosh-viewer"

ZIP_PATH="${DIST_DIR}/woosh-viewer-macos-universal.zip"
DMG_PATH="${DIST_DIR}/woosh-viewer-macos-universal.dmg"
CHECKSUM_PATH="${DIST_DIR}/woosh-viewer-macos-universal.sha256"
rm -f "${ZIP_PATH}" "${DMG_PATH}" "${CHECKSUM_PATH}"
ditto -c -k --sequesterRsrc --keepParent "${APP_PATH}" "${ZIP_PATH}"

DMG_ROOT="${STAGE_DIR}/dmg"
mkdir -p "${DMG_ROOT}"
cp -R "${APP_PATH}" "${DMG_ROOT}/Woosh Viewer.app"
ln -s /Applications "${DMG_ROOT}/Applications"
hdiutil create -volname "Woosh Viewer" -srcfolder "${DMG_ROOT}" -ov -format UDZO "${DMG_PATH}"

(
    cd "${DIST_DIR}"
    shasum -a 256 "$(basename "${ZIP_PATH}")" "$(basename "${DMG_PATH}")" \
        > "$(basename "${CHECKSUM_PATH}")"
)

echo "Created universal macOS packages:"
echo "  ${ZIP_PATH}"
echo "  ${DMG_PATH}"
echo "  ${CHECKSUM_PATH}"
