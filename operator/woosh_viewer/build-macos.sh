#!/bin/bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"
DIST_DIR="${SCRIPT_DIR}/dist"
DEPLOYMENT_TARGET="${MACOSX_DEPLOYMENT_TARGET:-12.0}"
ARM64_BINARY="${SCRIPT_DIR}/target/aarch64-apple-darwin/release/woosh-viewer"
X86_64_BINARY="${SCRIPT_DIR}/target/x86_64-apple-darwin/release/woosh-viewer"
PACKAGE_ONLY=0
SELECTED_ARCH=""
BINARY_OVERRIDE=""

while [[ $# -gt 0 ]]; do
    case "$1" in
        --package-only)
            PACKAGE_ONLY=1
            shift
            ;;
        --arch)
            SELECTED_ARCH="$2"
            shift 2
            ;;
        --binary)
            BINARY_OVERRIDE="$2"
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

if [[ -n "${SELECTED_ARCH}" && "${SELECTED_ARCH}" != "arm64" && "${SELECTED_ARCH}" != "x86_64" ]]; then
    echo "Unsupported architecture: ${SELECTED_ARCH} (use arm64 or x86_64)" >&2
    exit 2
fi
if [[ -n "${BINARY_OVERRIDE}" && -z "${SELECTED_ARCH}" ]]; then
    echo "--binary requires --arch" >&2
    exit 2
fi

if [[ -n "${SELECTED_ARCH}" ]]; then
    ARCHES=("${SELECTED_ARCH}")
else
    ARCHES=(arm64 x86_64)
fi

if [[ ${PACKAGE_ONLY} -eq 0 ]]; then
    for arch in "${ARCHES[@]}"; do
        if [[ "${arch}" == "arm64" ]]; then
            target="aarch64-apple-darwin"
        else
            target="x86_64-apple-darwin"
        fi
        rustup target add "${target}"
        (cd "${SCRIPT_DIR}" && cargo build --release --locked --target "${target}")
    done
fi

mkdir -p "${DIST_DIR}" "${SCRIPT_DIR}/target"
STAGE_DIR=""
cleanup() {
    if [[ -n "${STAGE_DIR}" && -d "${STAGE_DIR}" ]]; then
        case "${STAGE_DIR}" in
            "${SCRIPT_DIR}/target/macos-"*"-package."*) rm -rf "${STAGE_DIR}" ;;
            *)
                echo "Refusing to remove unexpected staging directory: ${STAGE_DIR}" >&2
                return 1
                ;;
        esac
    fi
}
trap cleanup EXIT

for arch in "${ARCHES[@]}"; do
    if [[ -n "${BINARY_OVERRIDE}" ]]; then
        binary="${BINARY_OVERRIDE}"
    elif [[ "${arch}" == "arm64" ]]; then
        binary="${ARM64_BINARY}"
    else
        binary="${X86_64_BINARY}"
    fi
    if [[ ! -f "${binary}" ]]; then
        echo "Missing macOS ${arch} binary: ${binary}" >&2
        exit 1
    fi

    if [[ "${arch}" == "arm64" ]]; then
        package_name="woosh-viewer-macos-arm64"
    else
        package_name="woosh-viewer-macos-intel-x64"
    fi

    STAGE_DIR="$(mktemp -d "${SCRIPT_DIR}/target/macos-${arch}-package.XXXXXX")"
    APP_PATH="${STAGE_DIR}/Woosh Viewer.app"
    CONTENTS_DIR="${APP_PATH}/Contents"
    MACOS_DIR="${CONTENTS_DIR}/MacOS"
    RESOURCES_DIR="${CONTENTS_DIR}/Resources"
    mkdir -p "${MACOS_DIR}" "${RESOURCES_DIR}"

    cp "${binary}" "${MACOS_DIR}/woosh-viewer"
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

    ZIP_PATH="${DIST_DIR}/${package_name}.zip"
    DMG_PATH="${DIST_DIR}/${package_name}.dmg"
    CHECKSUM_PATH="${DIST_DIR}/${package_name}.sha256"
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

    echo "Created ${arch} macOS packages:"
    echo "  ${ZIP_PATH}"
    echo "  ${DMG_PATH}"
    echo "  ${CHECKSUM_PATH}"
    cleanup
    STAGE_DIR=""
done
