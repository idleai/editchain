#!/usr/bin/env python3
"""Snapshot local Rust packages into one workspace for reproducible WASM builds.

Cargo hashes absolute paths for dependencies outside the selected workspace.
Flatten inherited manifest values and keep all local packages inside the build
workspace so compiler crate identities do not depend on the checkout directory.
Source manifests, dependency versions and per-package lint settings are retained.
See https://github.com/rust-lang/cargo/issues/7645.
"""

import copy
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tomllib


def read(path):
    return tomllib.loads(path.read_text())


def value(item):
    if isinstance(item, dict):
        return "{ " + ", ".join(json.dumps(key) + " = " + value(val) for key, val in item.items()) + " }"
    if isinstance(item, list):
        return "[" + ", ".join(value(val) for val in item) + "]"
    return json.dumps(item)


def write(path, document):
    text = "\n".join(json.dumps(key) + " = " + value(val) for key, val in document.items()) + "\n"
    if not path.exists() or path.read_text() != text:
        path.write_text(text)


def workspace(manifest):
    for directory in manifest.parent.parents:
        candidate = directory / "Cargo.toml"
        if candidate.is_file() and "workspace" in read(candidate):
            return directory, read(candidate)["workspace"]
    raise ValueError("No workspace for " + str(manifest))


def dependency_tables(document):
    for section in [document, *document.get("target", {}).values()]:
        for key in ["dependencies", "dev-dependencies", "build-dependencies"]:
            yield section.get(key, {})


def package(manifest):
    document = read(manifest)
    root, shared = workspace(manifest)
    inherited_files = {}
    for key, setting in list(document["package"].items()):
        if isinstance(setting, dict) and setting.get("workspace"):
            document["package"][key] = copy.deepcopy(shared["package"][key])
            if key in ["readme", "license-file"]:
                source = root / document["package"][key]
                relative = ".workspace/" + source.name
                inherited_files[relative] = source
                document["package"][key] = relative
    if document.get("lints", {}).get("workspace"):
        document["lints"] = copy.deepcopy(shared["lints"])
    for dependencies in dependency_tables(document):
        for name, setting in list(dependencies.items()):
            if not isinstance(setting, dict):
                continue
            base = manifest.parent
            if setting.get("workspace"):
                inherited = copy.deepcopy(shared["dependencies"][name])
                if isinstance(inherited, str):
                    inherited = {"version": inherited}
                features = inherited.get("features", []) + setting.get("features", [])
                inherited.update({key: val for key, val in setting.items() if key not in ["workspace", "features"]})
                if features:
                    inherited["features"] = list(dict.fromkeys(features))
                setting = dependencies[name] = inherited
                base = root
            if "path" in setting:
                setting["path"] = str((base / setting["path"]).resolve())
    return document, inherited_files


def stage(root):
    metadata = json.loads(subprocess.check_output(
        ["cargo", "metadata", "--manifest-path", str(root / "Cargo.toml"),
         "--locked", "--format-version", "1"], text=True))
    pending = [Path(item["manifest_path"]).resolve() for item in metadata["packages"]
               if item["source"] is None]
    packages = {}
    while pending:
        manifest = pending.pop()
        if manifest in packages:
            continue
        document, inherited_files = package(manifest)
        packages[manifest] = document, inherited_files
        for dependencies in dependency_tables(document):
            pending.extend(Path(setting["path"]) / "Cargo.toml" for setting in dependencies.values()
                           if isinstance(setting, dict) and "path" in setting)

    destination = root / "target" / "wasm-workspace"
    sources = destination / "packages"
    if sources.exists():
        shutil.rmtree(sources)
    sources.mkdir(parents=True)
    locations = {manifest: sources / document["package"]["name"]
                 for manifest, (document, _) in packages.items()}
    if len(set(locations.values())) != len(locations):
        raise ValueError("Local packages must have distinct names")
    for manifest, (document, inherited_files) in packages.items():
        directory = locations[manifest]
        shutil.copytree(manifest.parent, directory,
                        ignore=shutil.ignore_patterns("target", ".git", "node_modules"))
        for relative, source in inherited_files.items():
            target = directory / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(source, target)
        for dependencies in dependency_tables(document):
            for setting in dependencies.values():
                if isinstance(setting, dict) and "path" in setting:
                    target = locations[Path(setting["path"]) / "Cargo.toml"]
                    setting["path"] = os.path.relpath(target, directory)
        write(directory / "Cargo.toml", document)

    original = read(root / "Cargo.toml")
    combined = {key: original[key] for key in ["patch", "replace", "profile"] if key in original}
    combined["workspace"] = {
        "resolver": original["workspace"].get("resolver", "2"),
        "members": sorted(str(path.relative_to(destination)) for path in locations.values()),
    }
    write(destination / "Cargo.toml", combined)
    shutil.copy2(root / "Cargo.lock", destination / "Cargo.lock")
    # Former external packages become members, so their existing dev-dependency
    # edges enter the staged lockfile. Resolve those edges without changing any
    # locked package version or checksum.
    subprocess.run(["cargo", "metadata", "--manifest-path", str(destination / "Cargo.toml"),
                    "--offline", "--format-version", "1"], stdout=subprocess.DEVNULL, check=True)
    def locked_packages(path):
        return {(item["name"], item["version"], item.get("source"), item.get("checksum"))
                for item in read(path)["package"]}
    if locked_packages(root / "Cargo.lock") != locked_packages(destination / "Cargo.lock"):
        raise ValueError("The staged workspace changed the locked package versions")
    return destination


if __name__ == "__main__":
    print(stage(Path(sys.argv[1]).resolve()))
