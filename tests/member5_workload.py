"""Deterministic synthetic reader; measurements live outside deterministic output."""
import hashlib
import json
import os
from pathlib import Path
import resource
import sys
import time


def main():
    count, rounds = map(int, sys.argv[1:3])
    pause_seconds = float(sys.argv[3]) if len(sys.argv) > 3 else 0.0
    output = Path(os.environ["TEAMFS_OUTPUT"])
    began = time.perf_counter_ns()
    total = 0
    combined = hashlib.sha256()
    sleep_ns = 0
    for _ in range(rounds):
        for index in range(count):
            with open(f"data/{index:04d}.bin", "rb", buffering=0) as source:
                while chunk := source.read(65536):
                    combined.update(chunk)
                    total += len(chunk)
        if pause_seconds:
            sleep_start = time.perf_counter_ns()
            time.sleep(pause_seconds)
            sleep_ns += time.perf_counter_ns() - sleep_start
    body_ms = (time.perf_counter_ns() - began) / 1e6
    result = {"read_bytes": total, "sha256": combined.hexdigest()}
    (output / "result.json").write_text(json.dumps(result, sort_keys=True) + "\n")
    (output / "workload-measurement.json").write_text(json.dumps({
        "body_ms": body_ms,
        "sleep_requested_ms": pause_seconds * rounds * 1000,
        "sleep_observed_ms": sleep_ns / 1e6,
        "active_body_ms": body_ms - sleep_ns / 1e6,
        "self_maxrss_bytes": resource.getrusage(resource.RUSAGE_SELF).ru_maxrss * 1024,
        "scope": "body_ms covers open/read/SHA256/close and optional paced sleeps; active_body_ms subtracts observed sleep only, not FUSE blocking; excludes Python startup/output writes; self_maxrss is this Linux process only",
    }, indent=2) + "\n")


if __name__ == "__main__":
    main()
