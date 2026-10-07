"""Private real FUSE mount lifecycle shared by v0.3 checks and showcase."""
import errno
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import time

ROOT = Path(__file__).resolve().parents[1]
BINARY = Path(os.environ.get("CARGO_TARGET_DIR", str(Path.home() / ".cache/teamfs/target"))) / "debug/teamfs"


def fails(expected, operation):
    try:
        operation()
    except OSError as error:
        assert error.errno in expected, error
    else:
        raise AssertionError("operation unexpectedly succeeded")


def wait_for(predicate, seconds=10):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        if predicate():
            return
        time.sleep(.05)
    raise AssertionError("timed out waiting for condition")


class Mount:
    def __init__(self, label):
        self.base = Path(tempfile.mkdtemp(prefix=f"teamfs-{label}-", dir=Path.home()))
        self.mount = self.base / "mnt"
        self.mount.mkdir()
        self.store = self.base / "store"
        self.trace = (ROOT / f"artifacts/{label}-trace.log").open("w")
        self.process = None

    def mounted(self):
        return subprocess.run(["findmnt", "-rn", "--mountpoint", str(self.mount)],
                              stdout=subprocess.DEVNULL).returncode == 0

    def start(self, *options, binary=BINARY, trace=True):
        if self.process is not None or self.mounted():
            raise RuntimeError("test mount already owns a process/mount; stop it before restarting")
        args = ["--memory"] if "--memory" in options else ["--store", str(self.store)]
        args += [str(v) for v in options if v != "--memory"]
        self.process = subprocess.Popen([str(binary), "mount", str(self.mount), *(["--trace"] if trace else []), *args],
                                        stdout=self.trace, stderr=self.trace)
        def ready():
            assert self.process.poll() is None, "mount exited; see trace"
            return self.mounted()
        wait_for(ready)

    def stop(self, kill=False):
        if kill:
            self.process.kill()
            self.process.wait(timeout=10)
        subprocess.run(["fusermount3", "-u", str(self.mount)], check=True)
        code = self.process.wait(timeout=15)
        if not kill:
            assert code == 0, code
        self.process = None

    def cli(self, *args, success=True):
        result = subprocess.run([str(BINARY), *map(str, args)], capture_output=True, timeout=30)
        assert (result.returncode == 0) == success, (args, result.stdout, result.stderr)
        return result

    def sync(self):
        self.cli("sync", self.mount)

    def status(self):
        return json.loads((self.mount / ".teamfs/status.json").read_bytes())

    def trash(self):
        return json.loads(self.cli("trash", "list", self.mount, "--json").stdout)["entries"]

    def diff(self, snapshot):
        return json.loads(self.cli("snapshot", "diff", self.mount, snapshot, "--json").stdout)

    def snapshot(self, name):
        self.cli("snapshot", "create", self.mount, name)

    def purge(self, id="--all"):
        self.cli("trash", "purge", self.mount, id)

    def close(self):
        if self.process is not None:
            if self.process.poll() is None:
                self.process.kill()
            self.process.wait(timeout=10)
        if self.mounted():
            subprocess.run(["fusermount3", "-uz", str(self.mount)], check=True)
        self.trace.close()
        assert self.base.parent == Path.home() and self.base.name.startswith("teamfs-") and not self.mounted()
        shutil.rmtree(self.base)
