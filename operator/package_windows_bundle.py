"""Build a minimal source bundle for the Windows operator computer."""

from argparse import ArgumentParser
from hashlib import sha256
from pathlib import Path
import os
from zipfile import ZIP_DEFLATED, ZipFile, ZipInfo


REPO_ROOT = Path(__file__).resolve().parents[1]
BUNDLE_ROOT = "woosh-windows"
DEFAULT_OUTPUT = REPO_ROOT / "dist" / "woosh-windows-source.zip"

# Explicit allow-list: do not let robot maps, logs, caches, tests, Git history,
# credentials, or navigation code leak into the Windows transfer package.
FILES = {
    "README-WINDOWS.md": "README-WINDOWS.md",
    "LICENSE-APACHE": "LICENSE-APACHE",
    "LICENSE-MIT": "LICENSE-MIT",
    "operator/package_windows_bundle.py": "operator/package_windows_bundle.py",
    "operator/woosh_viewer/README.md": "operator/woosh_viewer/README.md",
    "operator/woosh_viewer/Cargo.lock": "operator/woosh_viewer/Cargo.lock",
    "operator/woosh_viewer/Cargo.toml": "operator/woosh_viewer/Cargo.toml",
    "operator/woosh_viewer/build-windows.ps1": "operator/woosh_viewer/build-windows.ps1",
    "operator/woosh_viewer/rust-toolchain.toml": "operator/woosh_viewer/rust-toolchain.toml",
    "operator/woosh_viewer/src/control_client.rs": "operator/woosh_viewer/src/control_client.rs",
    "operator/woosh_viewer/src/main.rs": "operator/woosh_viewer/src/main.rs",
    "operator/woosh_viewer/src/native_sidecar.rs": "operator/woosh_viewer/src/native_sidecar.rs",
    "operator/woosh_viewer/woosh-viewer.example.toml": "operator/woosh_viewer/woosh-viewer.example.toml",
}


def _zip_info(name):
    info = ZipInfo(f"{BUNDLE_ROOT}/{name}", date_time=(2026, 1, 1, 0, 0, 0))
    info.compress_type = ZIP_DEFLATED
    info.external_attr = 0o100644 << 16
    return info


def build_bundle(output):
    output = Path(output).resolve()
    missing = [source for source in FILES if not (REPO_ROOT / source).is_file()]
    if missing:
        raise FileNotFoundError("Windows bundle is missing required files: " + ", ".join(missing))

    output.parent.mkdir(parents=True, exist_ok=True)
    temporary = output.with_name(f".{output.name}.tmp")
    hashes = []
    source_bytes = 0
    try:
        with ZipFile(temporary, "w", compression=ZIP_DEFLATED, compresslevel=9) as archive:
            for source_name, target_name in sorted(FILES.items(), key=lambda item: item[1]):
                content = (REPO_ROOT / source_name).read_bytes()
                source_bytes += len(content)
                hashes.append(f"{sha256(content).hexdigest()}  {target_name}")
                archive.writestr(_zip_info(target_name), content, compresslevel=9)
            manifest = ("\n".join(hashes) + "\n").encode("utf-8")
            archive.writestr(_zip_info("BUNDLE-MANIFEST.sha256"), manifest, compresslevel=9)
        os.replace(temporary, output)
    finally:
        if temporary.exists():
            temporary.unlink()

    return {
        "output": output,
        "files": len(FILES),
        "source_bytes": source_bytes,
        "archive_bytes": output.stat().st_size,
    }


def verify_bundle(path):
    path = Path(path).resolve()
    manifest_name = f"{BUNDLE_ROOT}/BUNDLE-MANIFEST.sha256"
    expected_names = {f"{BUNDLE_ROOT}/{target}" for target in FILES.values()}
    expected_names.add(manifest_name)
    with ZipFile(path) as archive:
        names = set(archive.namelist())
        if names != expected_names:
            missing = sorted(expected_names - names)
            unexpected = sorted(names - expected_names)
            raise RuntimeError(
                f"Bundle content mismatch; missing={missing}, unexpected={unexpected}"
            )
        manifest = archive.read(manifest_name).decode("utf-8").splitlines()
        for line in manifest:
            expected_hash, target_name = line.split("  ", 1)
            content = archive.read(f"{BUNDLE_ROOT}/{target_name}")
            actual_hash = sha256(content).hexdigest()
            if actual_hash != expected_hash:
                raise RuntimeError(f"Bundle checksum mismatch: {target_name}")


def main(argv=None):
    parser = ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, default=DEFAULT_OUTPUT)
    args = parser.parse_args(argv)
    result = build_bundle(args.output)
    verify_bundle(result["output"])
    print(f"Created: {result['output']}")
    print(f"Files: {result['files']}")
    print(f"Source size: {result['source_bytes'] / 1024:.1f} KiB")
    print(f"ZIP size: {result['archive_bytes'] / 1024:.1f} KiB")
    print("Verification: passed")


if __name__ == "__main__":
    main()
