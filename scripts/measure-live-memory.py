#!/usr/bin/env python3
"""Measure a service's live open and paged reads against a read-only chain.

Usage: measure-live-memory.py BINARY WORKSPACE CHAIN OUTPUT_DIRECTORY
Run each binary against the same frozen segment copy. No provider capture,
source mutation, instrumentation clone, or change to a running editor is used.
Linux /proc supplies process RSS; timings include the real stdio transport.
"""

import argparse
import hashlib
import json
import pathlib
import subprocess
import threading
import time


def rss(pid):
    for line in pathlib.Path(f"/proc/{pid}/status").read_text().splitlines():
        if line.startswith("VmRSS:"):
            return int(line.split()[1])
    return 0


def request(process, body):
    payload = json.dumps({"id": 1, "body": body}).encode()
    start = time.monotonic()
    process.stdin.write(len(payload).to_bytes(4, "little") + payload)
    process.stdin.flush()
    prefix = process.stdout.read(4)
    if len(prefix) != 4:
        raise RuntimeError("service closed before responding")
    size = int.from_bytes(prefix, "little")
    payload = process.stdout.read(size)
    if len(payload) != size:
        raise RuntimeError("service returned a partial frame")
    elapsed = time.monotonic() - start
    response = json.loads(payload)["body"]
    if "Error" in response:
        raise RuntimeError(response["Error"])
    return response["Ok"], elapsed, size


def main():
    parser = argparse.ArgumentParser(description=__doc__, allow_abbrev=False)
    parser.add_argument('--paged', action='store_true')
    parser.add_argument('--append-command', nargs=argparse.REMAINDER,
                        help='Append to an owned fixture before SyncLive; place this option last')
    for name in ('binary', 'workspace', 'chain', 'destination'):
        parser.add_argument(name)
    args = parser.parse_args()
    binary, workspace, chain, destination = args.binary, args.workspace, args.chain, args.destination
    output = pathlib.Path(destination)
    output.mkdir(parents=True, exist_ok=True)
    samples = []
    stop = threading.Event()
    start = time.monotonic()
    with (output / "stderr.log").open("w") as stderr:
        process = subprocess.Popen([binary], stdin=subprocess.PIPE,
                                   stdout=subprocess.PIPE, stderr=stderr)

        def sample():
            while not stop.is_set():
                try:
                    samples.append([round(time.monotonic() - start, 3), rss(process.pid)])
                except FileNotFoundError:
                    break
                stop.wait(0.05)

        monitor = threading.Thread(target=sample, daemon=True)
        monitor.start()
        try:
            opened, elapsed, size = request(process, {"OpenLivePaged" if args.paged else "OpenLive": {
                "workspace_path": workspace, "chain_dir": chain}})
            topology = hashlib.sha256()
            for block in opened["live"]["blocks"]:
                topology.update(json.dumps(block, sort_keys=True).encode())
            result = {"binary": binary, "chain": chain, "open_seconds": elapsed,
                      "frame_bytes": size, "operations": opened["chain_generation"],
                      "rows": opened["nodes"], "blocks": len(opened["live"]["blocks"]),
                      "topology_sha256": topology.hexdigest(), "windows": [],
                      "paged": args.paged, "diagnostics": opened.get("diagnostics")}
            snapshot = opened["snapshot_id"]
            total = opened["nodes"]
            first, first_elapsed, first_bytes = request(process, {"GetWindow": {
                "snapshot_id": snapshot, "offset": 0, "limit": 500, "include_layout": True}})
            result.update(first_window_seconds=first_elapsed, first_window_bytes=first_bytes,
                          open_plus_first_window_seconds=result['open_seconds'] + first_elapsed)
            del first
            if args.paged:
                if args.append_command:
                    subprocess.run(args.append_command, check=True)
                update, sync_elapsed, sync_bytes = request(process, {"SyncLive": {
                    "epoch": opened["live"]["epoch"], "after_revision": opened["live"]["revision"], "codex": None}})
                result.update(sync_seconds=sync_elapsed, sync_bytes=sync_bytes, sync_work=update['work'])
                if update['deltas']:
                    snapshot = update['deltas'][-1]['snapshot_id']
                    total = update['deltas'][-1]['visible_total']
                result['rows_after_sync'] = total
                del update
            del opened
            for offset in sorted({0, 1, 31, total // 4, total // 2, 3 * total // 4,
                                  max(0, total - 32)}):
                window, elapsed, _ = request(process, {"GetWindow": {
                    "snapshot_id": snapshot, "offset": offset, "limit": 32,
                    "include_layout": True}})
                del window["snapshot_id"]
                result["windows"].append({"offset": offset, "seconds": elapsed,
                    "sha256": hashlib.sha256(json.dumps(window, sort_keys=True).encode()).hexdigest()})
            result["retained_rss_kib"] = rss(process.pid)
            result["peak_rss_kib"] = max(value for _, value in samples)
            (output / "summary.json").write_text(json.dumps(result, indent=2) + "\n")
            print(json.dumps(result, indent=2), flush=True)
        finally:
            stop.set()
            monitor.join()
            process.stdin.close()
            try:
                process.wait(timeout=30)
            except subprocess.TimeoutExpired:
                process.terminate()
                process.wait(timeout=10)
            (output / "samples.json").write_text(json.dumps(samples) + "\n")


if __name__ == "__main__":
    main()
