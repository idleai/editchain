#!/usr/bin/env python3
"""Build a producer's versioned native bundle and SHA-256 checksum."""

import argparse
import hashlib
import json
from pathlib import Path
import platform
import shutil
import subprocess
import tarfile
import tempfile
import tomllib


def run(root, *arguments):
    subprocess.run(arguments, cwd=root, check=True)


def output(root, *arguments):
    return subprocess.check_output(arguments, cwd=root, text=True)


def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def package_tests(root, config, staging):
    executables = []
    for package in config.get("test_packages", []):
        records = output(root, "cargo", "test", "--locked", "--release", "--all-features",
                         "--no-run", "--message-format=json", "-p", package)
        for line in records.splitlines():
            record = json.loads(line)
            if record.get("reason") != "compiler-artifact" or not record.get("executable"):
                continue
            if not record["profile"]["test"]:
                continue
            source = Path(record["executable"])
            name = record["target"]["name"] + "-" + record["target"]["kind"][0]
            relative = Path("tests") / (name + (".exe" if platform.system() == "Windows" else ""))
            destination = staging / relative
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(source, destination)
            executables.append(relative.as_posix())
    if config.get("test_packages") and not executables:
        raise RuntimeError("the native test build produced no test executables")
    (staging / "tests.json").write_text(json.dumps(sorted(executables), indent=2) + "\n")


def main():
    root = next(parent for parent in Path(__file__).resolve().parents
                if (parent / "native-release.json").is_file())
    config = json.loads((root / "native-release.json").read_text())
    manifest = tomllib.loads((root / "Cargo.toml").read_text())
    version = manifest.get("workspace", {}).get("package", {}).get("version")
    if version is None:
        version = manifest["package"]["version"]
    tag = f"{config['tag_prefix']}{version}"
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--tag", default=tag)
    parser.add_argument("--output", type=Path, default=root / "releases")
    args = parser.parse_args()
    if args.tag != tag:
        raise ValueError(f"release tag must match package version: {tag}")
    systems = {"Linux": "linux", "Darwin": "darwin", "Windows": "win32"}
    machines = {"x86_64": "x64", "amd64": "x64", "aarch64": "arm64", "arm64": "arm64"}
    target = f"{systems[platform.system()]}-{machines[platform.machine().lower()]}"
    run(root, "cargo", "build", "--release", "--locked", *config["build_args"])
    metadata = json.loads(output(root, "cargo", "metadata", "--locked", "--no-deps", "--format-version", "1"))
    release = Path(metadata["target_directory"]) / "release"
    suffix = ".exe" if platform.system() == "Windows" else ""
    args.output.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="idle-native-release-") as temporary:
        staging = Path(temporary)
        (staging / "bin").mkdir()
        for binary in config["binaries"]:
            source = release / (binary + suffix)
            shutil.copy2(source, staging / "bin" / source.name)
        for example in config.get("examples", []):
            source = release / "examples" / (example + suffix)
            shutil.copy2(source, staging / "bin" / source.name)
        package_tests(root, config, staging)
        for source, destination in config.get("files", {}).items():
            target_path = staging / destination
            target_path.parent.mkdir(parents=True, exist_ok=True)
            if (root / source).is_dir():
                shutil.copytree(root / source, target_path,
                                ignore=shutil.ignore_patterns("node_modules", "target", ".git", "dist"))
            else:
                shutil.copy2(root / source, target_path)
        bundle = {"schema": 1, "repository": config["repository"], "tag": tag,
                  "platform": target, "commit": output(root, "git", "rev-parse", "HEAD").strip(),
                  "files": {path.relative_to(staging).as_posix(): digest(path)
                            for path in sorted(staging.rglob("*")) if path.is_file()}}
        (staging / "bundle.json").write_text(json.dumps(bundle, indent=2) + "\n")
        archive = args.output / f"{tag}-{target}.tar.gz"
        with tarfile.open(archive, "w:gz") as tar:
            for path in sorted(staging.iterdir()):
                tar.add(path, arcname=path.name)
        archive.with_name(archive.name + ".sha256").write_text(f"{digest(archive)}  {archive.name}\n")
    print(archive)


if __name__ == "__main__":
    main()
