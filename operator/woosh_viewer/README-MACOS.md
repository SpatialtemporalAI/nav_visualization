# Woosh Viewer for macOS

Woosh Viewer is distributed as separate native applications for Apple Silicon
and Intel Macs. Each package contains the native Rust data service and does not
require Python or a separate sidecar.

## Install

1. Apple Silicon users open `woosh-viewer-macos-arm64.dmg`; Intel users open
   `woosh-viewer-macos-intel-x64.dmg`.
2. Drag **Woosh Viewer** to **Applications**.
3. Start the application and allow local-network access when macOS asks.
4. Open **连接设置**, enter the robot IP and port, then click the connection
   button.

Unsigned development builds use an ad-hoc signature. If Gatekeeper blocks the
first launch, Control-click the app in Finder, choose **Open**, then confirm.

Configuration and task history are stored outside the application bundle:

- `~/Library/Application Support/Woosh/woosh-viewer.toml`
- `~/Library/Application Support/Woosh/rerun-history`

## Build locally

Install Xcode Command Line Tools and Rust 1.95, then run:

```bash
cd operator/woosh_viewer
chmod +x build-macos.sh
./build-macos.sh
```

The script builds `aarch64-apple-darwin` and `x86_64-apple-darwin` separately,
applies an ad-hoc signature by default, and writes architecture-specific ZIP,
DMG, and SHA-256 files under `operator/woosh_viewer/dist`.

For a Developer ID signature, provide the identity before building:

```bash
MACOS_CODESIGN_IDENTITY="Developer ID Application: Example Corp (TEAMID)" \
  ./build-macos.sh
```
