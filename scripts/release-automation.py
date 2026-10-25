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


def candidate_ref(source):
    return f"refs/heads/ci/release/{source}"


def staged_candidate(source):
    ref = candidate_ref(source)
    result = subprocess.run(["git", "ls-remote", "--exit-code", "origin", ref],
                            text=True, capture_output=True, check=False)
    if result.returncode == 2:
        return None
    result.check_returncode()
    subprocess.run(["git", "fetch", "origin", ref], check=True)
    head = command("git", "rev-parse", "FETCH_HEAD")
    if not prepared_from(head, source):
        raise ValueError("staged release contains changes outside the version commit")
    return head


def promote(source, revision):
    """Advance main only after the reusable CI workflow verifies this commit."""
    if not all(re.fullmatch(r"[0-9a-f]{40}", value) for value in (source, revision)):
        raise ValueError("release revisions must be full commit IDs")
    if os.environ.get("VERIFIED_RELEASE_REVISION") != revision:
        raise ValueError("release promotion requires a successful CI result for this revision")
    if command("git", "rev-parse", "HEAD") != revision or command("git", "status", "--porcelain"):
        raise ValueError("release promotion requires a clean checkout of the verified revision")
    subprocess.run(["git", "fetch", "origin", "main"], check=True)
    current = command("git", "rev-parse", "origin/main")
    if current not in (source, revision):
        raise ValueError("a newer main commit superseded this release")
    if revision != source and not prepared_from(revision, source):
        raise ValueError("release promotion is limited to the prepared version commit")
    repository = os.environ["GITHUB_REPOSITORY"]
    run_id = os.environ["GITHUB_RUN_ID"]
    check = {"name": "CI required", "head_sha": revision,
             "status": "completed", "conclusion": "success",
             "details_url": f"https://github.com/{repository}/actions/runs/{run_id}",
             "output": {"title": "Release validation passed",
                        "summary": "The reusable CI workflow validated this exact release commit before promotion."}}
    subprocess.run(["gh", "api", "--method", "POST", f"repos/{repository}/check-runs", "--input", "-"],
                   input=json.dumps(check), text=True, check=True, stdout=subprocess.DEVNULL)
    if current != revision:
        # A concurrent main update rejects this normal push. Never rewrite main.
        subprocess.run(["git", "push", "origin", f"{revision}:refs/heads/main"], check=True)


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
    staged = staged_candidate(source)
    if staged:
        subprocess.run(["git", "checkout", "--detach", staged], check=True)
        output("revision", staged)
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
        # Keep main unchanged until the reusable CI workflow validates this commit.
        subprocess.run(["git", "push", "origin", f"HEAD:{candidate_ref(source)}"], check=True)
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
    promotion = subparsers.add_parser("promote")
    promotion.add_argument("--source", required=True)
    promotion.add_argument("--revision", required=True)
    subparsers.add_parser("artifact").add_argument("--package", required=True)
    args = parser.parse_args()
    os.chdir(Path(__file__).resolve().parent.parent)
    if args.command == "prepare":
        prepare(args.source)
    elif args.command == "promote":
        promote(args.source, args.revision)
    else:
        artifact(args.package)


if __name__ == "__main__":
    main()
