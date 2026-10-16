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
    output = Path(os.environ["TEAMFS_OUTPUT"])
    began = time.perf_counter_ns()
    total = 0
    combined = hashlib.sha256()
    for _ in range(rounds):
        for index in range(count):
            with open(f"data/{index:04d}.bin", "rb", buffering=0) as source:
                while chunk := source.read(65536):
                    combined.update(chunk)
                    total += len(chunk)
    body_ms = (time.perf_counter_ns() - began) / 1e6
    result = {"read_bytes": total, "sha256": combined.hexdigest()}
    (output / "result.json").write_text(json.dumps(result, sort_keys=True) + "\n")
    (output / "workload-measurement.json").write_text(json.dumps({
        "body_ms": body_ms,
        "self_maxrss_bytes": resource.getrusage(resource.RUSAGE_SELF).ru_maxrss * 1024,
        "scope": "body_ms covers open/read/SHA256/close loops; excludes Python startup and output writes; Linux self_maxrss covers this workload process only",
    }, indent=2) + "\n")


if __name__ == "__main__":
    main()
