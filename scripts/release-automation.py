#!/usr/bin/env python3
"""Prepare a version commit, or resume an unfinished artifact publication."""

import argparse
import json
import os
from pathlib import Path
import re
import subprocess
import tomllib


def command(*arguments):
    return subprocess.check_output(arguments, text=True).strip()


def output(name, value):
    print(f"{name}={value}")
    if os.environ.get("GITHUB_OUTPUT"):
        with open(os.environ["GITHUB_OUTPUT"], "a", encoding="utf-8") as stream:
            stream.write(f"{name}={value}\n")


def metadata_only(paths):
    return all(Path(path).name in {"Cargo.toml", "Cargo.lock", "CHANGELOG.md"}
               and not path.startswith(".github/") for path in paths)


def packages():
    config = tomllib.loads(Path("release-plz.toml").read_text())
    excluded = {item["name"] for item in config.get("package", []) if item.get("release") is False}
    metadata = json.loads(command("cargo", "metadata", "--no-deps", "--format-version", "1"))
    return [package for package in metadata["packages"] if package["name"] not in excluded]


def tag_commit(tag):
    result = subprocess.run(["git", "rev-parse", "--verify", f"refs/tags/{tag}^{{commit}}"],
                            text=True, capture_output=True, check=False)
    return result.stdout.strip() if result.returncode == 0 else None


def prepared_from(head, source):
    if command("git", "show", "-s", "--format=%P", head) != source:
        return False
    message = command("git", "show", "-s", "--format=%B", head)
    author = command("git", "show", "-s", "--format=%ae", head)
    paths = command("git", "diff", "--name-only", source, head).splitlines()
    return (f"Release-Source: {source}" in message.splitlines()
            and author == "41898282+github-actions[bot]@users.noreply.github.com"
            and bool(paths) and metadata_only(paths))


def prepare(source):
    if not re.fullmatch(r"[0-9a-f]{40}", source):
        raise ValueError("source must be a full commit ID")
    if command("git", "status", "--porcelain"):
        raise ValueError("release preparation requires a clean checkout")
    subprocess.run(["git", "fetch", "origin", "main", "--tags"], check=True)
    current = command("git", "rev-parse", "origin/main")
    if current != source:
        if prepared_from(current, source):
            output("revision", current)
        else:
            print("A newer main commit owns the next release")
            output("revision", "")
        return
    subprocess.run(["git", "checkout", "--detach", source], check=True)
    subprocess.run(["release-plz", "update"], check=True)
    paths = command("git", "ls-files", "--modified", "--others", "--exclude-standard").splitlines()
    if not metadata_only(paths):
        raise ValueError(f"release-plz changed files outside release metadata: {paths}")
    if paths:
        subprocess.run(["git", "add", "--", *paths], check=True)
        subprocess.run(["git", "-c", "user.name=github-actions[bot]", "-c",
                        "user.email=41898282+github-actions[bot]@users.noreply.github.com", "commit",
                        "-m", "chore: prepare package releases", "-m", f"Release-Source: {source}"], check=True)
        # A normal push rejects a concurrent main update. Never rewrite main.
        subprocess.run(["git", "push", "origin", "HEAD:refs/heads/main"], check=True)
    head = command("git", "rev-parse", "HEAD")
    pending = any(tag_commit(f"{package['name']}-v{package['version']}") in (None, head)
                  for package in packages())
    output("revision", head if paths or pending else "")


def artifact(package_name):
    package = next(package for package in packages() if package["name"] == package_name)
    tag = f"{package_name}-v{package['version']}"
    # Existing public artifacts are immutable. A retry resumes only a draft at this commit.
    revision = tag_commit(tag)
    if revision is None:
        raise ValueError(f"release-plz did not create the artifact tag: {tag}")
    if revision != command("git", "rev-parse", "HEAD"):
        output("tag", "")
        return
    release = json.loads(command("gh", "release", "view", tag, "--json", "isDraft"))
    output("tag", tag if release["isDraft"] else "")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="command", required=True)
    subparsers.add_parser("prepare").add_argument("--source", required=True)
    subparsers.add_parser("artifact").add_argument("--package", required=True)
    args = parser.parse_args()
    os.chdir(Path(__file__).resolve().parent.parent)
    if args.command == "prepare":
        prepare(args.source)
    else:
        artifact(args.package)


if __name__ == "__main__":
    main()
