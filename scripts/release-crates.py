#!/usr/bin/env python3
"""Package Cargo crates and publish their immutable GitHub release index."""

import argparse
import base64
import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import tempfile
import tomllib

CRATES_IO = "https://github.com/rust-lang/crates.io-index"


def command(root, *arguments):
    return subprocess.check_output(arguments, cwd=root, text=True)


def api(root, repository, resource, payload=None, missing=False):
    args = ["gh", "api", f"repos/{repository}/{resource}"]
    if payload is not None:
        args += ["--method", "POST", "--input", "-"]
    result = subprocess.run(args, cwd=root, input=None if payload is None else json.dumps(payload),
                            text=True, capture_output=True, check=False)
    if result.returncode:
        if missing and "HTTP 404" in result.stderr:
            return None
        raise RuntimeError(result.stderr.strip())
    return json.loads(result.stdout)


def crate_path(name):
    name = name.lower()
    if len(name) < 3:
        return f"{len(name)}/{name}"
    if len(name) == 3:
        return f"3/{name[0]}/{name}"
    return f"{name[:2]}/{name[2:4]}/{name}"


def index_entry(package, checksum, registry):
    dependencies = []
    for dependency in package["dependencies"]:
        source = dependency.get("registry") or CRATES_IO
        if dependency.get("source", "") and dependency["source"].startswith("git+"):
            raise ValueError(f"{package['name']} has a Git dependency: {dependency['name']}")
        entry = {"name": dependency.get("rename") or dependency["name"],
                 "req": dependency["req"], "features": dependency["features"],
                 "optional": dependency["optional"],
                 "default_features": dependency["uses_default_features"],
                 "target": dependency["target"], "kind": dependency["kind"] or "normal",
                 "registry": None if source == registry else source}
        if dependency.get("rename"):
            entry["package"] = dependency["name"]
        dependencies.append(entry)
    return {"name": package["name"], "vers": package["version"], "deps": dependencies,
            "cksum": checksum, "features": {}, "features2": package["features"],
            "v": 2, "yanked": False, "links": package.get("links"),
            "rust_version": package.get("rust_version")}


def index_snapshot(root, repository, packages, config):
    reference = api(root, repository, "git/ref/heads/cargo-index", missing=True)
    if reference is None:
        tree = api(root, repository, "git/trees", {"tree": [
            {"path": "config.json", "mode": "100644", "type": "blob",
             "content": json.dumps(config) + "\n"}]})
        commit = api(root, repository, "git/commits", {
            "message": "Initialize Cargo package index", "tree": tree["sha"], "parents": []})
        reference = api(root, repository, "git/refs", {
            "ref": "refs/heads/cargo-index", "sha": commit["sha"]})
    parent = reference["object"]["sha"]
    previous = {}
    for package in packages:
        content = api(root, repository, f"contents/{crate_path(package['name'])}?ref={parent}", missing=True)
        previous[package["name"]] = "" if content is None else base64.b64decode(content["content"]).decode()
    return parent, previous


def publish(root, repository, parent, index, packages, output):
    # Archives become public before the atomic index update makes them resolvable.
    for package in packages:
        name, version = package["name"], package["version"]
        tag = f"{name}-v{version}"
        # The tag endpoint omits draft releases; the CLI resolves their numeric IDs.
        details = json.loads(command(root, "gh", "release", "view", tag, "--repo", repository, "--json", "apiUrl"))
        release_id = details["apiUrl"].rsplit("/", 1)[1]
        release = api(root, repository, f"releases/{release_id}")
        archive = output / f"{name}-{version}.crate"
        assets = {asset["name"]: asset for asset in release["assets"]}
        for path in (archive, archive.with_name(archive.name + ".sha256")):
            if path.name in assets:
                with tempfile.TemporaryDirectory(prefix="idle-release-check-") as temporary:
                    subprocess.run(["gh", "release", "download", tag, "--pattern", path.name,
                                    "--dir", temporary, "--repo", repository], cwd=root, check=True)
                    existing = Path(temporary) / path.name
                    if existing.read_bytes() != path.read_bytes():
                        raise ValueError(f"cannot replace existing release asset: {path.name}")
            else:
                subprocess.run(["gh", "release", "upload", tag, str(path), "--repo", repository],
                               cwd=root, check=True)
        if release["draft"]:
            subprocess.run(["gh", "release", "edit", tag, "--draft=false", "--repo", repository],
                           cwd=root, check=True)
    commit = api(root, repository, f"git/commits/{parent}")
    files = []
    for path in sorted(index.rglob("*")):
        if not path.is_file():
            continue
        blob = api(root, repository, "git/blobs", {
            "content": base64.b64encode(path.read_bytes()).decode(), "encoding": "base64"})
        files.append({"path": path.relative_to(index).as_posix(), "mode": "100644",
                      "type": "blob", "sha": blob["sha"]})
    tree = api(root, repository, "git/trees", {"base_tree": commit["tree"]["sha"], "tree": files})
    new_commit = api(root, repository, "git/commits", {
        "message": "Publish Cargo package versions", "tree": tree["sha"], "parents": [parent]})
    subprocess.run(["gh", "api", f"repos/{repository}/git/refs/heads/cargo-index", "--method", "PATCH",
                    "--input", "-"], input=json.dumps({"sha": new_commit["sha"], "force": False}),
                   cwd=root, text=True, check=True, stdout=subprocess.DEVNULL)


def main():
    root = Path(__file__).resolve().parent.parent
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, default=root / "releases")
    parser.add_argument("--allow-dirty", action="store_true")
    parser.add_argument("--publish", action="store_true")
    args = parser.parse_args()
    if args.publish and args.allow_dirty:
        raise ValueError("publication requires a committed checkout")
    metadata = json.loads(command(root, "cargo", "metadata", "--no-deps", "--format-version", "1"))
    packages = [package for package in metadata["packages"] if package["publish"]]
    registries = {registry for package in packages for registry in package["publish"]}
    if len(registries) != 1:
        raise ValueError("each producer must publish its crates to exactly one named registry")
    registry_name = registries.pop()
    registry = tomllib.loads((root / ".cargo/config.toml").read_text())["registries"][registry_name]["index"]
    repository = packages[0]["repository"].removeprefix("https://github.com/")
    config = {"dl": f"https://github.com/{repository}/releases/download/{{crate}}-v{{version}}/{{crate}}-{{version}}.crate"}
    parent, previous = index_snapshot(root, repository, packages, config) if args.publish else (None, {})
    pending = [package for package in packages if not any(
        json.loads(line)["vers"] == package["version"] for line in previous.get(package["name"], "").splitlines())]
    if not pending:
        print("All package versions are already indexed")
        return
    if args.publish:
        head = command(root, "git", "rev-parse", "HEAD").strip()
        for package in pending:
            tag = f"{package['name']}-v{package['version']}"
            reference = api(root, repository, f"git/ref/tags/{tag}")["object"]
            while reference["type"] == "tag":
                reference = api(root, repository, f"git/tags/{reference['sha']}")["object"]
            if reference["sha"] != head:
                raise ValueError(f"check out the tagged commit before publishing {tag}")
    arguments = ["cargo", "package", "--workspace", "--locked", "--all-features", "--registry", registry_name]
    for package in metadata["packages"]:
        if not package["publish"]:
            arguments += ["--exclude", package["name"]]
    if args.allow_dirty:
        arguments.append("--allow-dirty")
    subprocess.run(arguments, cwd=root, check=True)
    index = args.output / "index"
    if index.exists():
        shutil.rmtree(index)
    index.mkdir(parents=True, exist_ok=True)
    (index / "config.json").write_text(json.dumps(config) + "\n")
    for package in pending:
        archive = args.output / f"{package['name']}-{package['version']}.crate"
        shutil.copyfile(Path(metadata["target_directory"]) / "package" / archive.name, archive)
        checksum = hashlib.sha256(archive.read_bytes()).hexdigest()
        archive.with_name(archive.name + ".sha256").write_text(f"{checksum}  {archive.name}\n")
        entry = index_entry(package, checksum, registry)
        destination = index / crate_path(package["name"])
        destination.parent.mkdir(parents=True, exist_ok=True)
        destination.write_text(previous.get(package["name"], "") + json.dumps(entry, separators=(",", ":")) + "\n")
    if args.publish:
        publish(root, repository, parent, index, pending, args.output)
    print(f"Packaged {len(pending)} crates in {args.output}")


if __name__ == "__main__":
    main()
