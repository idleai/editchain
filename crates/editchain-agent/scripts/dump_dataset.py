#!/usr/bin/env python3
"""Dump a HuggingFace SWE-bench dataset to JSONL for Rust consumption."""

import json
import sys

from datasets import load_dataset


def main():
    if len(sys.argv) < 3:
        print(f"Usage: {sys.argv[0]} <dataset_name> <split> [output_file]", file=sys.stderr)
        print(f"  If output_file is omitted, writes to stdout.", file=sys.stderr)
        sys.exit(1)

    dataset_name = sys.argv[1]
    split = sys.argv[2]
    output_file = sys.argv[3] if len(sys.argv) > 3 else None

    print(f"Loading {dataset_name} split={split}...", file=sys.stderr)
    dataset = load_dataset(dataset_name, split=split)
    print(f"Loaded {len(dataset)} instances", file=sys.stderr)

    fh = open(output_file, "w") if output_file else sys.stdout
    with fh:
        for instance in dataset:
            # Select only the fields we need
            record = {
                "instance_id": instance["instance_id"],
                "problem_statement": instance["problem_statement"],
                "repo": instance.get("repo"),
                "base_commit": instance.get("base_commit"),
                "hint": instance.get("hint"),
                "created_at": instance.get("created_at"),
                "fail_to_pass": instance.get("fail_to_pass"),
                "pass_to_pass": instance.get("pass_to_pass"),
                "image_name": instance.get("image_name"),
                "docker_image": instance.get("docker_image"),
            }
            print(json.dumps(record), file=fh)

    if output_file:
        print(f"Wrote {len(dataset)} instances to {output_file}", file=sys.stderr)


if __name__ == "__main__":
    main()