"""Request attribution, access classes, paths, errors and capture scope on real mounts.

Each case runs real commands through `teamfs experiment run`. The daemon's `--trace` lines
(one per FUSE callback, with the calling thread and the bytes in the reply) are rebuilt into
per-file counts and must equal the report exactly, so the report is checked against the
requests the daemon actually received rather than against itself.
"""
import collections
import errno
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tarfile
from fs_harness import Mount, BINARY, ROOT, wait_for
from reporting import Report, EnvironmentSkip, Unsupported

report = Report("attribution-acceptance")
TRACE = re.compile(r"^\[\d+\] (\S+)\s+ino=(\d+) pid=(\d+) (\{.*\}) => (?:OK(?: bytes=(\d+))?|errno=(\d+) .*)$")
OBSERVED = {"read", "open", "lookup", "getattr", "readlink", "readdir", "opendir"}
RAW = b"data/raw-\xff\n.bin"


def tool(name):
    if not shutil.which(name):
        raise EnvironmentSkip(f"missing executable: {name}")
    return name


class Lab:
    def __init__(self, mode):
        self.h = Mount("attribution-" + mode)
        self.mode = mode
        self.m = self.h.mount
        self.h.start(*(["--cached-io"] if mode == "cached" else []), "--no-audit-file")

    def put(self, relative, data):
        path = self.m / os.fsdecode(relative)
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)
        # cached-io: publish page-cache writes before the input snapshot is taken.
        fd = os.open(path, os.O_RDONLY)
        os.fsync(fd)
        os.close(fd)

    def run(self, name, *command, success=True, during=None):
        """Run an experiment; `during(outputs)` executes while the command is still running."""
        out = self.h.base / ("report-" + name)
        start = self.h.trace_path.stat().st_size
        job = subprocess.Popen([str(BINARY), "experiment", "run", str(self.m), name, "--output", str(out), "--", *command],
                               stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        if during:
            during(out / "outputs")
        stdout, stderr = job.communicate(timeout=60)
        assert (job.returncode == 0) == success, (name, job.returncode, stdout.decode(), stderr.decode())
        with self.h.trace_path.open("rb") as trace:
            trace.seek(start)
            lines = [TRACE.match(line.decode(errors="replace")) for line in trace.read().splitlines()]
        manifest = json.loads((out / "manifest.json").read_bytes())
        return out, manifest, stderr.decode(), [m.groups() for m in lines if m]

    def pinned(self, name):
        """Inode -> path bytes for every node of the experiment's snapshot (queried after the run)."""
        base = self.m / ".teamfs/snapshots" / ("experiment." + name)
        nodes = {base.lstat().st_ino: b""}
        for parent, dirs, files in os.walk(base):
            for entry in dirs + files:
                path = Path(parent) / entry
                nodes[path.lstat().st_ino] = os.fsencode(path.relative_to(base))
        return nodes, (self.m / ".teamfs/snapshots").lstat().st_ino

    def delete(self, name):
        self.h.cli("experiment", "delete", self.m, name)

    def close(self):
        self.h.close()


def single(manifest):
    """A single-threaded command that starts no children: its only thread is the group leader."""
    return {manifest["process_group"]: manifest["process_group"]}


def rows(manifest):
    capture = manifest["capture"]
    return {bytes(r["path_bytes"]): r for r in capture["inputs"] + capture["metadata_accesses"]}


def reconcile(lab, name, manifest, lines, group=None):
    """Rebuild the capture from raw callbacks.

    `group` maps every thread ID of the experiment to its process ID, as reported by the program
    itself. Without it (tools that fork helpers), any caller except this harness is treated as
    a group member and per-node process lists are not compared.
    """
    member = (lambda tid: tid in group) if group is not None else (lambda tid: tid != os.getpid())
    pinned, snapshots_ino = lab.pinned(name)
    nodes = collections.defaultdict(lambda: {"operations": collections.Counter(), "read_calls": 0, "read_bytes": 0, "tids": set()})
    errors = collections.Counter()
    other = collections.Counter()
    for op, ino, tid, detail, nbytes, err in lines:
        ino, tid, detail = int(ino), int(tid), json.loads(detail)
        if op not in OBSERVED:
            continue
        if op == "lookup":
            # The callback names the parent; the report attributes a successful lookup to the child.
            name_bytes = bytes(detail["name_bytes"])
            if ino == snapshots_ino and name_bytes == os.fsencode("experiment." + name):
                path = b""
            elif ino in pinned:
                path = pinned[ino] + b"/" + name_bytes if pinned[ino] else name_bytes
            else:
                continue
        elif ino in pinned:
            path = pinned[ino]
        else:
            continue
        if not member(tid):
            readable = op == "open" and detail.get("flags", 0) & os.O_ACCMODE != os.O_WRONLY
            if err is None and (op in ("read", "readlink") or readable):
                other[tid] += 1
            continue
        if err is not None:
            errors[(path, op, int(err))] += 1
            continue
        node = nodes[path]
        node["operations"][op] += 1
        node["tids"].add(tid)
        if op == "read":
            node["read_calls"] += 1
            node["read_bytes"] += int(nbytes)
    capture, reported = manifest["capture"], rows(manifest)
    assert set(reported) == set(nodes), ("report and callbacks disagree on accessed nodes", set(reported) ^ set(nodes))
    table = []
    for path, node in sorted(nodes.items()):
        row = reported[path]
        assert row["operations"] == dict(node["operations"]), (path, row["operations"], node["operations"])
        assert (row["read_calls"], row["read_bytes"]) == (node["read_calls"], node["read_bytes"]), (path, row)
        if group is not None:
            assert row["pids"] == sorted({group[t] for t in node["tids"]}), (path, row["pids"], node["tids"])
        table.append({"path": row["path_display"], "access_class": row["access_class"],
                      "callback_operations": dict(node["operations"]), "callback_read_bytes": node["read_bytes"],
                      "callback_thread_ids": sorted(node["tids"]), "report_pids": row["pids"],
                      "report_read_calls": row["read_calls"], "report_read_bytes": row["read_bytes"]})
    reported_errors = {(bytes(e["path_bytes"]), e["operation"], e["errno"]): e["count"] for e in capture["errors"]}
    assert reported_errors == dict(errors), (reported_errors, errors)
    assert capture["other_group_snapshot_accesses"] == sum(other.values()), (capture["other_group_snapshot_accesses"], other)
    assert set(capture["other_group_pids"]) <= set(other)
    return {"nodes": table, "errors": [{"path": os.fsdecode(p), "operation": o, "errno": e, "count": c} for (p, o, e), c in sorted(errors.items())],
            "other_group_content_requests": {str(k): v for k, v in sorted(other.items())}, "callbacks_examined": len(lines)}


PROGRAM = r'''
import json, os, subprocess, sys, threading, time
from pathlib import Path
out = Path(os.environ["TEAMFS_OUTPUT"])
reads, errors = {}, {}
def consume(path, chunks):
    fd = os.open(path, os.O_RDONLY)
    got = [len(os.pread(fd, size, offset)) for offset, size in chunks]
    os.close(fd)
    return [len(chunks), sum(got)]
reads["data/样本 1.csv"] = consume("data/样本 1.csv", [(0, 100), (1000, 500), (2900, 4096)])
reads[b"data/raw-\xff\n.bin".hex()] = consume(b"data/raw-\xff\n.bin", [(16, 4)])
size = os.stat("params.json").st_size
reads["params.json"] = consume("link-to-params", [(0, 4096), (size, 4096)])
reads["empty.txt"] = consume("empty.txt", [(0, 10)])
os.close(os.open("opened-only.txt", os.O_RDONLY))
os.stat("stat-only.txt")
listed = sorted(os.listdir("listing"))
try:
    open("data/missing.csv")
except OSError as e:
    errors["data/missing.csv"] = e.errno
tid = {}
def worker():
    tid["value"] = threading.get_native_id()
    reads["thread-input.txt"] = consume("thread-input.txt", [(0, 64)])
thread = threading.Thread(target=worker); thread.start(); thread.join()
child = """
import os, subprocess, sys
from pathlib import Path
out = Path(os.environ["TEAMFS_OUTPUT"])
fd = os.open("child-input.txt", os.O_RDONLY); n = len(os.pread(fd, 32, 0)); os.close(fd)
grandchild = "import os; from pathlib import Path; fd = os.open('grandchild-input.txt', os.O_RDONLY); n = len(os.pread(fd, 16, 0)); os.close(fd); Path(os.environ['TEAMFS_OUTPUT'], 'grandchild.json').write_text(str([os.getpid(), n]))"
subprocess.run([sys.executable, "-c", grandchild], check=True)
(out / "child.json").write_text(str([os.getpid(), n]))
"""
subprocess.run([sys.executable, "-c", child], check=True)
leaver = "import os; from pathlib import Path; fd = os.open('setsid-input.txt', os.O_RDONLY); os.pread(fd, 8, 0); os.close(fd); Path(os.environ['TEAMFS_OUTPUT'], 'setsid.json').write_text(str([os.getpid(), os.getpgid(0)]))"
subprocess.run([sys.executable, "-c", leaver], check=True, start_new_session=True)
(out / "ready").touch()
while not (out / "go").exists():
    time.sleep(0.01)
child_pid, child_bytes = json.loads((out / "child.json").read_text())
grandchild_pid, grandchild_bytes = json.loads((out / "grandchild.json").read_text())
reads["child-input.txt"] = [1, child_bytes]
reads["grandchild-input.txt"] = [1, grandchild_bytes]
setsid_pid, setsid_group = json.loads((out / "setsid.json").read_text())
(out / "accounting.json").write_text(json.dumps({"pid": os.getpid(), "pgid": os.getpgid(0), "thread_tid": tid["value"],
    "child": child_pid, "grandchild": grandchild_pid, "setsid": setsid_pid, "setsid_group": setsid_group,
    "reads": reads, "errors": errors, "listed": listed}))
'''


def classified_run(mode):
    lab = Lab(mode)
    try:
        lab.put("probe_program.py", PROGRAM.encode())
        lab.put("data/样本 1.csv", b"".join(b"%04d,%s\n" % (i, b"x" * 24) for i in range(100)))
        lab.put(RAW, bytes(range(256)) * 16)
        lab.put("params.json", b'{"scale": 2, "offset": 1.5}\n')
        lab.put("empty.txt", b"")
        lab.put("opened-only.txt", b"opened but never read\n")
        lab.put("stat-only.txt", b"only stat\n")
        lab.put("backup/unused-backup.csv", b"never touched\n")
        for i in range(3):
            lab.put(f"listing/item-{i}.csv", b"listed only\n")
        lab.put("thread-input.txt", bytes(range(64)))
        lab.put("child-input.txt", b"c" * 32)
        lab.put("grandchild-input.txt", b"g" * 16)
        lab.put("setsid-input.txt", b"left the process group\n")
        lab.put("outside-read.txt", b"read by another process\n")
        os.symlink("params.json", lab.m / "link-to-params")
        assert (lab.m / "data/样本 1.csv").stat().st_size == 3000
        snapshot = lab.m / ".teamfs/snapshots/experiment.classified"

        def outside(outputs):
            wait_for(lambda: (outputs / "ready").exists(), 30)
            for _ in range(3):
                assert (snapshot / "outside-read.txt").read_bytes() == b"read by another process\n"
            (outputs / "go").touch()
        out, manifest, stderr, lines = lab.run("classified", "python3", "probe_program.py", during=outside)
        app = json.loads((out / "outputs/accounting.json").read_text())
        capture = manifest["capture"]
        assert app["pgid"] == manifest["process_group"] == app["pid"] and app["setsid_group"] != app["pgid"]
        group = {app["pid"]: app["pid"], app["thread_tid"]: app["pid"], app["child"]: app["child"], app["grandchild"]: app["grandchild"]}
        crosscheck = reconcile(lab, "classified", manifest, lines, group)
        by_path, inputs = rows(manifest), {bytes(r["path_bytes"]): r for r in capture["inputs"]}
        # 1. Classes: content, opened-only, attribute and directory queries are separate.
        content = {b"probe_program.py", "data/样本 1.csv".encode(), RAW, b"params.json", b"empty.txt",
                   b"thread-input.txt", b"child-input.txt", b"grandchild-input.txt"}
        assert {p for p, r in inputs.items() if r["access_class"] == "content_read"} == content
        assert {p for p, r in inputs.items() if r["access_class"] == "opened_only"} == {b"opened-only.txt"}
        expected_metadata = {b"": "directory_listing", b"listing": "directory_listing", b"data": "attribute_only",
                             b"stat-only.txt": "attribute_only", b"link-to-params": "link_read"}
        assert {bytes(r["path_bytes"]): r["access_class"] for r in capture["metadata_accesses"]} == expected_metadata
        for never in (b"backup", b"backup/unused-backup.csv", b"listing/item-0.csv", b"setsid-input.txt", b"outside-read.txt"):
            assert never not in by_path, never
        assert inputs[b"empty.txt"]["read_calls"] == 1 and inputs[b"empty.txt"]["read_bytes"] == 0
        assert inputs[b"empty.txt"]["sha256"] == hashlib.sha256(b"").hexdigest()
        # Counts the program itself performed equal the report.
        for key, (calls, nbytes) in app["reads"].items():
            path = bytes.fromhex(key) if key == RAW.hex() else key.encode()
            assert (inputs[path]["read_calls"], inputs[path]["read_bytes"]) == (calls, nbytes), (key, inputs[path])
        # 3. Partial reads still identify the whole file version.
        for path, row in inputs.items():
            assert row["sha256"] == hashlib.sha256((snapshot / os.fsdecode(path)).read_bytes()).hexdigest(), path
        assert inputs["data/样本 1.csv".encode()]["read_bytes"] == 700 < 3000
        # 2. Attribution: threads count as their process; children are included; others never.
        assert inputs[b"thread-input.txt"]["pids"] == [app["pid"]] and app["thread_tid"] != app["pid"]
        assert inputs[b"child-input.txt"]["pids"] == [app["child"]]
        assert inputs[b"grandchild-input.txt"]["pids"] == [app["grandchild"]]
        assert capture["summary"]["processes"] == sorted({app["pid"], app["child"], app["grandchild"]})
        assert {app["setsid"], os.getpid()} <= set(capture["other_group_pids"])
        assert capture["other_group_snapshot_accesses"] >= 4
        assert "本进程组之外的进程" in stderr
        # 4. Missing input errno.
        assert app["errors"] == {"data/missing.csv": errno.ENOENT}
        assert capture["errors"] == [{"path_bytes": list(b"data/missing.csv"), "path_display": "data/missing.csv",
                                      "operation": "lookup", "errno": errno.ENOENT, "count": 1}]
        assert capture["all_observed_content_inputs_pinned"] and capture["unpinned_mount_accesses"] == 0
        verified = lab.h.cli("experiment", "verify", lab.m, out).stdout.decode()
        assert "验证通过：9 个文件" in verified and "注意：" in verified
        summary = {k: v for k, v in capture["summary"].items() if k != "processes"}
        assert summary == {"content_read": 8, "opened_only": 1, "attribute_only": 2, "directory_listing": 2, "link_read": 1, "error_requests": 1}
        report.context.setdefault("request_report_crosscheck", {})[mode] = {
            "program_accounting": app, "report_summary": capture["summary"], **crosscheck}
        (ROOT / "artifacts/attribution-manifest-example.json").write_text(json.dumps(manifest, ensure_ascii=False, indent=2) + "\n")
        lab.delete("classified")
        return f"{crosscheck['callbacks_examined']} callbacks reconciled; {len(crosscheck['nodes'])} nodes, other-group requests {capture['other_group_snapshot_accesses']}"
    finally:
        lab.close()


def beyond_window():
    lab = Lab("direct")
    try:
        original = bytes((i * 7) % 251 for i in range(8192))
        lab.put("window.bin", original)
        program = """import json, os, time
from pathlib import Path
out = Path(os.environ["TEAMFS_OUTPUT"])
fd = os.open("window.bin", os.O_RDONLY)
total = sum(len(os.pread(fd, 1 + i % 61, (i * 37) % 8000)) for i in range(2600))
(out / "half").touch()
while not (out / "go").exists():
    time.sleep(0.01)
total += sum(len(os.pread(fd, 1 + i % 61, (i * 37) % 8000)) for i in range(2600, 2700))
os.close(fd)
(out / "accounting.json").write_text(json.dumps({"reads": 2700, "bytes": total}))
"""
        evicted_before = lab.h.status()["audit"]["evicted"]

        def modify(outputs):
            wait_for(lambda: (outputs / "half").exists(), 60)
            # Same-named business file changes while the pinned version is being read.
            (lab.m / "window.bin").write_bytes(b"changed" * 100)
            (outputs / "go").touch()
        out, manifest, _, lines = lab.run("window", "python3", "-c", program, during=modify)
        app = json.loads((out / "outputs/accounting.json").read_text())
        status = lab.h.status()["audit"]
        crosscheck = reconcile(lab, "window", manifest, lines, single(manifest))
        row = {bytes(r["path_bytes"]): r for r in manifest["capture"]["inputs"]}[b"window.bin"]
        assert (row["read_calls"], row["read_bytes"]) == (app["reads"], app["bytes"]) == (2700, app["bytes"])
        assert status["evicted"] - evicted_before > 0 and status["retained"] < app["reads"], status
        assert row["sha256"] == hashlib.sha256(original).hexdigest() != hashlib.sha256((lab.m / "window.bin").read_bytes()).hexdigest()
        lab.h.cli("experiment", "verify", lab.m, out)
        report.context["beyond_audit_window"] = {"program": app, "report_read_calls": row["read_calls"], "report_read_bytes": row["read_bytes"],
                                                 "audit_window_retained": status["retained"], "audit_evicted_during_mount": status["evicted"],
                                                 "callbacks_examined": crosscheck["callbacks_examined"]}
        lab.delete("window")
        return f"2700 variable-size reads, {app['bytes']} bytes, exact after the {status['retained']}-event audit window evicted {status['evicted']}"
    finally:
        lab.close()


def names_and_bytes():
    lab = Lab("direct")
    try:
        names = {"数据集/第一组/样本 A.csv".encode(): (b"a,b\n1,2\n" * 50, "数据集/第一组/样本 A.csv"),
                 b"raw-\xff\xfe\n\tname\\.bin": (bytes(range(256)) * 4, "raw-\\xff\\xfe\\n\\tname\\\\.bin"),
                 "空文件.txt".encode(): (b"", "空文件.txt")}
        for path, (data, _) in names.items():
            lab.put(path, data)
        program = ("import os\nf=os.open('数据集/第一组/样本 A.csv',os.O_RDONLY); assert len(os.read(f,4096))==400; os.close(f)\n"
                   "f=os.open(b'raw-\\xff\\xfe\\n\\tname\\\\.bin',os.O_RDONLY); assert os.pread(f,3,255)==bytes([255,0,1]); os.close(f)\n"
                   "f=os.open('空文件.txt',os.O_RDONLY); assert os.read(f,8)==b''; os.close(f)")
        out, manifest, _, lines = lab.run("names", "python3", "-c", program)
        reconcile(lab, "names", manifest, lines, single(manifest))
        inputs = {bytes(r["path_bytes"]): r for r in manifest["capture"]["inputs"]}
        assert set(inputs) == set(names)
        for path, (data, display) in names.items():
            row = inputs[path]
            assert row["path_display"] == display, (row["path_display"], display)
            assert row["sha256"] == hashlib.sha256(data).hexdigest() and row["access_class"] == "content_read"
        assert inputs[b"raw-\xff\xfe\n\tname\\.bin"]["read_bytes"] == 3
        shown = json.loads(lab.h.cli("experiment", "show", out).stdout)
        assert shown == manifest
        lab.h.cli("experiment", "verify", lab.m, out)
        lab.delete("names")
        return "nested Chinese directories, invalid UTF-8, newline/tab/backslash names, binary partial read, empty file"
    finally:
        lab.close()


def errors_and_kernel_side_failures():
    lab = Lab("direct")
    try:
        lab.put("data.txt", b"data\n")
        lab.put("sub/present.txt", b"present\n")
        program = ("import json,os,errno\nfrom pathlib import Path\nseen={}\n"
                   "for p in ['missing.csv','sub/missing.csv','data.txt/x']:\n try: open(p).read()\n except OSError as e: seen[p]=e.errno\n"
                   "try: os.open('data.txt',os.O_WRONLY)\nexcept OSError as e: seen['write data.txt']=e.errno\n"
                   "Path(os.environ['TEAMFS_OUTPUT'],'seen.json').write_text(json.dumps(seen))")
        out, manifest, _, lines = lab.run("errors", "python3", "-c", program)
        seen = json.loads((out / "outputs/seen.json").read_text())
        reconcile(lab, "errors", manifest, lines, single(manifest))
        capture = manifest["capture"]
        errors = {(e["path_display"], e["operation"]): e["errno"] for e in capture["errors"]}
        assert seen["missing.csv"] == seen["sub/missing.csv"] == errno.ENOENT
        assert errors[("missing.csv", "lookup")] == errors[("sub/missing.csv", "lookup")] == errno.ENOENT
        # ENOTDIR for a file used as a directory, and default_permissions denials, are decided
        # by the kernel and never reach FUSE: they must not be invented in the report.
        assert seen["data.txt/x"] == errno.ENOTDIR and not any(p.startswith("data.txt/") for p, _ in errors)
        if os.getuid() == 0:
            assert seen["write data.txt"] == errno.EROFS and errors[("data.txt", "open")] == errno.EROFS
        else:
            assert seen["write data.txt"] == errno.EACCES and ("data.txt", "open") not in errors
        row = rows(manifest)[b"data.txt"]
        assert row["access_class"] == "attribute_only" and "open" not in row["operations"]
        lab.delete("errors")
        return f"application saw {seen}; report holds only the FUSE-visible errors {sorted(errors)}"
    finally:
        lab.close()


def unpinned_inputs():
    lab = Lab("direct")
    try:
        lab.put("business.txt", b"mutable business file\n")
        lab.put("other.txt", b"other snapshot\n")
        lab.h.snapshot("other")
        lab.put("doomed.txt", b"in trash\n")
        (lab.m / "doomed.txt").unlink()
        trash_id = lab.h.trash()[0]["id"]
        program = (f"open({str(lab.m / 'business.txt')!r}).read(); open('../../../business.txt').read()\n"
                   f"open('../other/other.txt').read(); open({str(lab.m / '.teamfs/trash' / str(trash_id))!r}).read()")
        out, manifest, stderr, lines = lab.run("unpinned", "python3", "-c", program)
        group = single(manifest)
        reconcile(lab, "unpinned", manifest, lines, group)
        capture = manifest["capture"]
        files = {(r["location"], r["path_display"]): r for r in capture["unpinned_mount_files"]}
        assert set(files) == {("business", "business.txt"), ("snapshot", ".teamfs/snapshots/other/other.txt"), ("trash", f".teamfs/trash/{trash_id}")}
        assert files[("business", "business.txt")]["operations"]["open"] == 2
        # Rebuild the unpinned counts from callbacks: content requests by the group outside its snapshot.
        pinned, _ = lab.pinned("unpinned")
        content = [l for l in lines if int(l[2]) in group and int(l[1]) not in pinned and l[5] is None and
                   (l[0] == "read" or l[0] == "open" and json.loads(l[3])["flags"] & os.O_ACCMODE != os.O_WRONLY)]
        assert capture["unpinned_mount_accesses"] == len(content) == sum(sum(r["operations"].values()) for r in files.values())
        assert not capture["all_observed_content_inputs_pinned"] and "business:business.txt" in stderr
        failed = lab.h.cli("experiment", "verify", lab.m, out, success=False).stderr.decode()
        assert "business:business.txt" in failed and f"trash:.teamfs/trash/{trash_id}" in failed
        lab.delete("unpinned")
        return f"{capture['unpinned_mount_accesses']} unpinned requests listed by location and path; verify refuses"
    finally:
        lab.close()


def link_boundaries():
    lab = Lab("direct")
    try:
        lab.put("params.json", b"{}\n")
        lab.put("business.txt", b"business\n")
        (lab.h.base / "outside.txt").write_text("outside the mount\n")
        links = {"chain-a": "chain-b", "chain-b": "params.json", "dangling": "nothing-here",
                 "abs-external": str(lab.h.base / "outside.txt"), "abs-mount": str(lab.m / "business.txt"),
                 "rel-escape": "../../../../outside.txt"}
        for link, target in links.items():
            os.symlink(target, lab.m / link)
        program = "for p in %r:\n try: open(p).read()\n except OSError: pass" % sorted(links)
        out, manifest, stderr, lines = lab.run("links", "python3", "-c", program)
        reconcile(lab, "links", manifest, lines, single(manifest))
        capture = manifest["capture"]
        meta = {r["path_display"]: r for r in capture["metadata_accesses"]}
        escaping = {name for name, row in meta.items() if row.get("link_may_escape_input")}
        assert escaping == {"abs-external", "abs-mount", "rel-escape"}, escaping
        assert all(meta[name]["access_class"] == "link_read" for name in links)
        assert capture["external_link_accesses"] == sum(meta[n]["operations"]["readlink"] for n in escaping)
        assert {r["path_display"] for r in capture["inputs"]} == {"params.json"}
        assert [(e["path_display"], e["errno"]) for e in capture["errors"]] == [("nothing-here", errno.ENOENT)]
        assert [(r["location"], r["path_display"]) for r in capture["unpinned_mount_files"]] == [("business", "business.txt")]
        failed = lab.h.cli("experiment", "verify", lab.m, out, success=False).stderr.decode()
        assert "rel-escape" in failed and "abs-external" in failed and "软链接" in stderr
        lab.delete("links")
        return "in-tree chains certified; absolute, into-mount and ../ escapes flagged; dangling target errno kept"
    finally:
        lab.close()


def mmap_boundary(kind):
    def case():
        lab = Lab("direct")
        try:
            lab.put("mapped.bin", os.urandom(100000))
            flags = "flags=mmap.MAP_PRIVATE,prot=mmap.PROT_READ" if kind == "private" else "access=mmap.ACCESS_READ"
            program = (f"import json,mmap,os\nfrom pathlib import Path\nf=os.open('mapped.bin',os.O_RDONLY)\nresult={{}}\n"
                       f"try:\n m=mmap.mmap(f,0,{flags}); result['touched']=len(m[:100]); m.close()\n"
                       f"except OSError as e: result['errno']=e.errno\nPath(os.environ['TEAMFS_OUTPUT'],'r.json').write_text(json.dumps(result))")
            out, manifest, _, lines = lab.run("mmap-" + kind, "python3", "-c", program)
            result = json.loads((out / "outputs/r.json").read_text())
            reconcile(lab, "mmap-" + kind, manifest, lines, single(manifest))
            row = {r["path_display"]: r for r in manifest["capture"]["inputs"]}["mapped.bin"]
            lab.delete("mmap-" + kind)
            if kind == "shared":
                if result.get("errno") == errno.ENODEV:
                    assert row["access_class"] == "opened_only"
                    raise Unsupported("shared mmap of pinned inputs fails with ENODEV (direct I/O); the open is still recorded")
                raise AssertionError(f"shared mmap behaviour changed: {result}; add capture tests before claiming support")
            assert result == {"touched": 100} and row["access_class"] == "content_read"
            assert row["read_bytes"] >= 100 and row["read_calls"] >= 1
            return f"private read-only mapping touched 100 bytes; page-cache readahead requested {row['read_bytes']} bytes in {row['read_calls']} read(s)"
        finally:
            lab.close()
    return case


def tools_in_experiments():
    lab = Lab("direct")
    results = {}
    try:
        lab.put("notes.txt", "实验记录\nline 2\n".encode())
        lab.put("big.bin", os.urandom(100000))
        lab.put("empty.dat", b"")
        lab.put("tree/a.txt", b"alpha\n")
        lab.put("tree/b.csv", b"1,2\n")
        os.symlink("a.txt", lab.m / "tree/link")
        repo = lab.m / "repo"; repo.mkdir()
        git = lambda *a: subprocess.run([tool("git"), "-c", "core.autocrlf=false", "-c", "commit.gpgsign=false", *a], cwd=repo, check=True, capture_output=True)
        git("init", "-q", "-b", "main"); git("config", "user.name", "TeamFS"); git("config", "user.email", "t@example.invalid")
        (repo / "报告.txt").write_text("pinned commit\n"); git("add", "."); git("commit", "-qm", "pinned message")

        def run(name, *command):
            out, manifest, _, lines = lab.run(name, *command)
            reconcile(lab, name, manifest, lines)
            classes = {r["path_display"]: r["access_class"] for r in manifest["capture"]["inputs"] + manifest["capture"]["metadata_accesses"]}
            return out, manifest, classes

        out, manifest, classes = run("tool-ls", tool("ls"), "-laR", "tree")
        assert not manifest["capture"]["inputs"] and classes["tree"] == "directory_listing" and classes["tree/link"] == "link_read"
        assert classes["tree/a.txt"] == "attribute_only"
        lab.delete("tool-ls"); results["ls -laR"] = "metadata only"
        out, manifest, classes = run("tool-find", tool("find"), "tree", "-name", "*.csv")
        assert not manifest["capture"]["inputs"] and classes["tree"] == "directory_listing"
        lab.delete("tool-find"); results["find"] = "directory listing only"
        out, manifest, classes = run("tool-stat", tool("stat"), "notes.txt")
        assert classes["notes.txt"] == "attribute_only" and not manifest["capture"]["inputs"]
        lab.delete("tool-stat"); results["stat"] = "attribute only"
        out, manifest, classes = run("tool-head", tool("head"), "-c", "10", "big.bin")
        row = manifest["capture"]["inputs"][0]
        assert row["path_display"] == "big.bin" and row["access_class"] == "content_read" and 10 <= row["read_bytes"] < 100000
        assert row["sha256"] == hashlib.sha256((lab.m / "big.bin").read_bytes()).hexdigest()
        lab.delete("tool-head"); results["head -c 10"] = f"partial read {row['read_bytes']} bytes, full-file SHA256"
        out, manifest, classes = run("tool-wc", tool("wc"), "-c", "big.bin")
        row = manifest["capture"]["inputs"][0]
        assert (out / "stdout.txt").read_text().split()[0] == "100000"
        assert row["access_class"] == ("content_read" if row["read_calls"] else "opened_only")
        lab.delete("tool-wc"); results["wc -c"] = f"{row['access_class']} ({row['read_calls']} reads; size from fstat)"
        out, manifest, classes = run("tool-sha256sum", tool("sha256sum"), "notes.txt", "empty.dat", "big.bin")
        sums = {line.split()[1]: line.split()[0] for line in (out / "stdout.txt").read_text().splitlines()}
        inputs = {r["path_display"]: r for r in manifest["capture"]["inputs"]}
        assert {name: inputs[name]["sha256"] for name in sums} == sums and all(r["access_class"] == "content_read" for r in inputs.values())
        lab.delete("tool-sha256sum"); results["sha256sum"] = "tool digests equal report SHA256, empty file is a 0-byte read"
        archive = lab.h.base / "tree.tar"
        out, manifest, classes = run("tool-tar", tool("tar"), "-cf", str(archive), "tree")
        with tarfile.open(archive) as tar:
            assert tar.extractfile("tree/a.txt").read() == b"alpha\n" and tar.getmember("tree/link").linkname == "a.txt"
        inputs = {r["path_display"]: r for r in manifest["capture"]["inputs"]}
        assert {k: v["access_class"] for k, v in inputs.items()} == {"tree/a.txt": "content_read", "tree/b.csv": "content_read"}
        lab.delete("tool-tar"); results["tar"] = "archived files content_read, link recorded as link_read"
        copy = lab.h.base / "rsync-copy"
        out, manifest, classes = run("tool-rsync", tool("rsync"), "-a", "tree/", f"{copy}/")
        assert (copy / "a.txt").read_bytes() == b"alpha\n" and os.readlink(copy / "link") == "a.txt"
        subprocess.run(["chmod", "-R", "u+w", str(copy)], check=True)  # rsync -a kept the snapshot's read-only modes
        assert {r["path_display"]: r["access_class"] for r in manifest["capture"]["inputs"]} == {"tree/a.txt": "content_read", "tree/b.csv": "content_read"}
        lab.h.cli("experiment", "verify", lab.m, out)
        lab.delete("tool-rsync"); results["rsync -a"] = "copy byte-exact; inputs content_read; verify passes"
        out, manifest, classes = run("tool-git", tool("git"), "-C", "repo", "--no-optional-locks", "show", "HEAD:报告.txt")
        assert (out / "stdout.txt").read_text() == "pinned commit\n"
        inputs = {r["path_display"]: r for r in manifest["capture"]["inputs"]}
        assert inputs["repo/.git/HEAD"]["access_class"] == "content_read" and not manifest["capture"]["unpinned_mount_accesses"]
        lab.h.cli("experiment", "verify", lab.m, out)
        probes = sorted({e["path_display"] for e in manifest["capture"]["errors"]})
        lab.delete("tool-git"); results["git show"] = f"{len(inputs)} repository files read; {len(probes)} optional paths probed with errno"
        copy = lab.h.base / "vim-copy.txt"
        out, manifest, classes = run("tool-vim", tool("vim"), "-Nu", "NONE", "-i", "NONE", "-n", "-es", "-c", f"w! {copy}", "-c", "q!", "notes.txt")
        assert copy.read_bytes() == (lab.m / "notes.txt").read_bytes()
        assert {r["path_display"]: r["access_class"] for r in manifest["capture"]["inputs"]}["notes.txt"] == "content_read"
        lab.delete("tool-vim"); results["vim -es"] = "read pinned file, wrote copy outside the mount"
        report.context["tools_in_experiments"] = results
        return results
    finally:
        lab.close()


report.case("direct / request-report reconciliation, classes, threads, children, other groups", lambda: classified_run("direct"))
report.case("cached / request-report reconciliation, classes, threads, children, other groups", lambda: classified_run("cached"))
report.case("2700 reads beyond the audit window while the same-named file changes", beyond_window)
report.case("Chinese, invalid UTF-8, control-character names and partial binary reads", names_and_bytes)
report.case("missing inputs keep errno; kernel-decided failures are not invented", errors_and_kernel_side_failures)
report.case("unpinned business, snapshot and trash reads are listed and block verify", unpinned_inputs)
report.case("symlink chains, escapes and dangling targets", link_boundaries)
report.case("private mmap of a pinned input", mmap_boundary("private"))
report.case("shared mmap of a pinned input", mmap_boundary("shared"))
report.case("ls, find, stat, head, wc, sha256sum, tar, rsync, git and vim as experiment commands", tools_in_experiments)
report.finish()
