#!/usr/bin/env python3
"""Publish native bundles, preserving assets already attached to a release."""

import argparse
import hashlib
import json
from pathlib import Path
import subprocess

PLATFORMS = ("linux-x64", "darwin-x64", "darwin-arm64", "win32-x64")


def publish(repository, tag, directory, *, all_platforms=False, extension="tar.gz"):
    release = json.loads(subprocess.check_output([
        "gh", "release", "view", tag, "--repo", repository, "--json", "isDraft,assets"], text=True))
    if not release["isDraft"] and not all_platforms:
        raise ValueError("automatic publication can only complete a draft release")
    existing = {asset["name"] for asset in release["assets"]}
    uploads = []
    for platform in PLATFORMS if all_platforms else ("linux-x64",):
        archive = directory / f"{tag}-{platform}.{extension}"
        checksum = archive.with_name(archive.name + ".sha256")
        present = {archive.name, checksum.name} & existing
        if len(present) == 2:
            continue
        if present:
            raise ValueError(f"release has an incomplete existing archive/checksum pair for {platform}")
        if not archive.is_file() or not checksum.is_file():
            raise ValueError(f"missing native bundle or checksum for {platform}")
        expected, filename = checksum.read_text(encoding="utf-8").strip().split()
        with archive.open("rb") as stream:
            actual = hashlib.file_digest(stream, "sha256").hexdigest()
        if filename != archive.name or expected != actual:
            raise ValueError(f"invalid native bundle checksum for {platform}")
        uploads.extend((str(archive), str(checksum)))
    # Validate the complete requested set before changing the release. Never overwrite an asset.
    if uploads:
        subprocess.run(["gh", "release", "upload", tag, "--repo", repository, *uploads], check=True)
    if release["isDraft"]:
        subprocess.run(["gh", "release", "edit", tag, "--repo", repository, "--draft=false"], check=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repository", required=True)
    parser.add_argument("--tag", required=True)
    parser.add_argument("--directory", type=Path, default=Path("releases"))
    parser.add_argument("--all-platforms", action="store_true")
    parser.add_argument("--extension", choices=("tar.gz", "vsix"), default="tar.gz")
    args = parser.parse_args()
    publish(args.repository, args.tag, args.directory, all_platforms=args.all_platforms, extension=args.extension)


if __name__ == "__main__":
    main()
