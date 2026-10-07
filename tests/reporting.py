"""Explicit results: unsupported and environment skips are never counted as passes."""
import atexit
import collections
import json
import os
import platform
import subprocess
import time
import traceback
from fs_harness import ROOT


class EnvironmentSkip(Exception):
    pass


class Unsupported(Exception):
    pass


class Report:
    def __init__(self, name):
        self.name = name
        self.rows = []
        self.context = {}
        atexit.register(self._incomplete)

    def case(self, name, action):
        began = time.perf_counter()
        detail = None
        try:
            detail = action()
            state = "pass"
        except EnvironmentSkip as e:
            state, detail = "environment_skip", str(e)
        except Unsupported as e:
            state, detail = "unsupported", str(e)
        except Exception:
            state, detail = "fail", traceback.format_exc()
        row = dict(name=name, status=state, seconds=time.perf_counter()-began, detail=detail)
        self.rows.append(row)
        print(f"{state.upper()}: {name}", flush=True)
        if state != "pass":
            print(detail, flush=True)
        return state

    def _incomplete(self):
        self._write(False)

    def _write(self, completed):
        counts = dict(collections.Counter(r["status"] for r in self.rows))
        result = dict(schema_version=1, measured_at=time.strftime("%Y-%m-%dT%H:%M:%S%z"),
                      suite=self.name, completed=completed, environment=dict(kernel=platform.release(), platform=platform.platform(),
                      python=platform.python_version(), uid=os.getuid()), context=self.context,
                      summary=counts, cases=self.rows)
        path = ROOT / "artifacts" / f"{self.name}.json"
        path.write_text(json.dumps(result, ensure_ascii=False, indent=2)+"\n")
        if not completed:
            return result
        print(f"RESULT {counts}; report: {path}", flush=True)
        return result

    def finish(self):
        result = self._write(True)
        atexit.unregister(self._incomplete)
        counts = result["summary"]
        failed = counts.get("fail", 0) or (os.environ.get("TEAMFS_REQUIRE_TOOLS") == "1" and counts.get("environment_skip", 0))
        if failed:
            raise SystemExit(1)
        return result
