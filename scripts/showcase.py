"""保存、快照、恢复的真实演示；演示存储只在该次运行中使用。"""
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import time

root = Path(__file__).resolve().parents[1]
binary = Path(os.environ["CARGO_TARGET_DIR"]) / "debug/teamfs"
base = Path(tempfile.mkdtemp(prefix="teamfs-showcase-v2-", dir=Path.home()))
mount = base / "mnt"
store = base / "store"
mount.mkdir()
trace = (root / "artifacts/showcase-trace.log").open("w")
process = None


def is_mounted():
    return subprocess.run(["findmnt", "-rn", "--mountpoint", str(mount)], stdout=subprocess.DEVNULL).returncode == 0


def start():
    global process
    process = subprocess.Popen([str(binary), "mount", str(mount), "--store", str(store), "--trace"], stdout=trace, stderr=trace)
    for _ in range(200):
        if process.poll() is not None:
            raise RuntimeError("挂载失败，查看 showcase-trace.log")
        if is_mounted():
            return
        time.sleep(.05)
    raise RuntimeError("挂载超时")


def stop():
    global process
    subprocess.run(["fusermount3", "-u", str(mount)], check=True)
    assert process.wait(timeout=15) == 0
    process = None


def cli(*args):
    subprocess.run([str(binary), *map(str, args)], check=True, timeout=30)


def step(text):
    print("\n" + text, flush=True)


try:
    print("TeamFS v0.2：一份报告的保存与恢复", flush=True)
    print(time.strftime("%Y-%m-%dT%H:%M:%S%z"), flush=True)
    start()
    report = mount / "notes/report.txt"
    original = "小组报告初稿\n我们用 Rust 与 fuse-rs 实现可恢复的资料文件系统。\n".encode()
    step("1. 创建报告，并显式同步到 SQLite")
    report.write_bytes(original)
    print(report.read_text(), flush=True)
    cli("sync", mount)
    step("2. 卸载并重新挂载：报告仍然存在")
    stop()
    start()
    assert report.read_bytes() == original
    print("PASS：重新挂载后的报告逐字节一致。", flush=True)
    step("3. 创建 before-edit 快照")
    begin = time.perf_counter()
    cli("snapshot", "create", mount, "before-edit")
    seconds = time.perf_counter() - begin
    cli("snapshot", "list", mount)
    step("4. 覆盖当前报告；通过普通 cat 读取只读历史目录")
    report.write_text("误改后的内容\n")
    history = mount / ".teamfs/snapshots/before-edit/notes/report.txt"
    subprocess.run(["cat", str(history)], check=True)
    assert history.read_bytes() == original and report.read_bytes() != original
    step("5. 恢复为 report-recovered.txt，不覆盖当前报告")
    cli("restore", mount, "before-edit", "notes/report.txt", "notes/report-recovered.txt")
    recovered = mount / "notes/report-recovered.txt"
    subprocess.run(["cmp", str(history), str(recovered)], check=True)
    digest = hashlib.sha256(recovered.read_bytes()).hexdigest()
    print("PASS：cmp 内容一致；SHA256 =", digest, flush=True)
    cli("sync", mount)
    step("6. cat .teamfs/status.json：查看当前状态")
    subprocess.run(["cat", str(mount / ".teamfs/status.json")], check=True)
    state = json.loads((mount / ".teamfs/status.json").read_bytes())
    measured = {"measured_at": time.strftime("%Y-%m-%dT%H:%M:%S%z"), "mode": "persistent", "fixture": "seed files plus one UTF-8 report", "snapshot_command_seconds": seconds, "report_bytes": len(original), "snapshot_content_bytes": state["snapshot_bytes"], "backend_files_bytes_before_unmount": sum(p.stat().st_size for p in store.iterdir() if p.is_file()), "recovered_sha256": digest}
    stop()
    measured["backend_files_bytes_after_unmount"] = sum(p.stat().st_size for p in store.iterdir() if p.is_file())
    start()
    assert recovered.read_bytes() == original and history.read_bytes() == original
    stop()
    (root / "artifacts/showcase-metrics.json").write_text(json.dumps(measured, ensure_ascii=False, indent=2) + "\n")
    print("\nPASS：恢复文件与快照跨重挂载保留。", flush=True)
    print(json.dumps(measured, ensure_ascii=False, indent=2), flush=True)
    print("以上是当前小文件样例的一次实测，不代表通用性能结论。演示挂载和临时存储将清理。", flush=True)
finally:
    if process is not None and process.poll() is None:
        process.kill()
        process.wait(timeout=10)
    if is_mounted():
        subprocess.run(["fusermount3", "-uz", str(mount)], check=True)
    trace.close()
    assert base.parent == Path.home() and base.name.startswith("teamfs-showcase-v2-") and not is_mounted()
    shutil.rmtree(base)
