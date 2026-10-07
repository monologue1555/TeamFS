"""Real TeamFS 0.3 demonstration; never uses the user's playground."""
import hashlib
import json
import os
from pathlib import Path
import platform
import subprocess
import sys
import time

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "tests"))
from fs_harness import Mount, ROOT, BINARY, wait_for

h = Mount("showcase")
m = h.mount


def step(message):
    print("\n" + message, flush=True)


def cli(*args):
    result = h.cli(*args)
    print(result.stdout.decode(), end="", flush=True)
    return result


def disk_bytes():
    return sum(p.stat().st_size for p in h.store.iterdir() if p.is_file())


try:
    print("TeamFS 基础功能演示：忘记保存、误改、误删，再找回", flush=True)
    print(time.strftime("%Y-%m-%dT%H:%M:%S%z"), flush=True)
    print("binary:", BINARY, flush=True)
    print("kernel:", platform.release(), flush=True)
    h.start("--auto-sync", 1)
    report = m / "notes/report.txt"
    draft = "小组报告初稿\nRust + FUSE：文件请求交给我们的程序处理。\n".encode()
    step("1. 创建报告，不手动同步；演示开启 --auto-sync 1")
    begin = time.perf_counter()
    report.write_bytes(draft)
    wait_for(lambda: h.status()["auto_sync_successes"] >= 1 and not h.status()["dirty"])
    auto_wait = time.perf_counter() - begin
    print(f"PASS：自动保存完成，从写入到观测成功 {auto_wait:.6f}s（含定时等待）。", flush=True)

    step("2. 强制结束挂载进程，再加载同一存储")
    h.stop(kill=True)
    h.start("--auto-sync", 1)
    assert report.read_bytes() == draft
    print("PASS：异常退出后，自动同步的报告逐字节保留。", flush=True)

    step("3. 建立 before-edit 快照，修改报告，查看差异")
    begin = time.perf_counter()
    h.snapshot("before-edit")
    snapshot_seconds = time.perf_counter() - begin
    report.write_bytes("误改后的内容\n".encode())
    cli("snapshot", "diff", m, "before-edit")
    diff = h.diff("before-edit")
    assert any(c["path_bytes"] == list(b"notes/report.txt") and c["content_changed"] for c in diff["changes"])
    (ROOT / "artifacts/showcase-diff.json").write_text(json.dumps(diff, ensure_ascii=False, indent=2) + "\n")
    cli("restore", m, "before-edit", "notes/report.txt", "notes/report-recovered.txt")
    history = m / ".teamfs/snapshots/before-edit/notes/report.txt"
    restored_report = m / "notes/report-recovered.txt"
    subprocess.run(["cmp", str(history), str(restored_report)], check=True)
    print("PASS：快照恢复到新路径，内容与初稿一致。", flush=True)

    step("4. 创建一份不在快照里的新笔记，随后误删")
    note = m / "notes/new-note.bin"
    note_bytes = bytes(range(256)) + "没有预先建立快照的新笔记".encode()
    note.write_bytes(note_bytes)
    os.utime(note, (1700000000,1700000001))
    note.unlink()
    records = h.trash()
    record = records[-1]
    assert record["path_bytes"] == list(b"notes/new-note.bin")
    cli("trash", "list", m)
    assert not (m / ".teamfs/snapshots/before-edit/notes/new-note.bin").exists()

    step("5. 从回收站恢复二进制笔记，验证逐字节一致")
    cli("trash", "restore", m, record["id"], "notes/note-recovered.bin")
    recovered = m / "notes/note-recovered.bin"
    assert recovered.read_bytes() == note_bytes
    assert (m / ".teamfs/trash" / str(record["id"])).read_bytes() == note_bytes
    note_hash = hashlib.sha256(note_bytes).hexdigest()
    print("PASS：恢复内容一致，SHA256 =", note_hash, flush=True)

    step("6. 模拟编辑器通过改名替换报告，旧目标进入回收站")
    old_report = report.read_bytes()
    temp = m / "notes/editor-temp"
    temp.write_bytes("编辑器替换的新版本\n".encode())
    temp.rename(report)
    replaced = h.trash()[-1]
    assert replaced["reason"] == "rename_replace"
    assert (m / ".teamfs/trash" / str(replaced["id"])).read_bytes() == old_report
    cli("trash", "list", m)
    begin = time.perf_counter()
    h.sync()
    sync_seconds = time.perf_counter() - begin
    state = h.status()
    before_purge = disk_bytes()

    step("7. 清理回收站，正常卸载并重挂载")
    cli("trash", "purge", m, "--all")
    h.stop()
    after_unmount = disk_bytes()
    h.start()
    assert not h.trash()
    assert recovered.read_bytes() == note_bytes and restored_report.read_bytes() == draft
    print("PASS：清理结果已保存，恢复出的报告和笔记仍在。", flush=True)
    h.sync()  # Verification reads update atime; commit those timestamps before showing a clean status.
    print((m / ".teamfs/status.json").read_text(), flush=True)
    h.stop()
    metrics = {
        "measured_at": time.strftime("%Y-%m-%dT%H:%M:%S%z"),
        "teamfs_version": state["version"], "rust_version": subprocess.check_output(["rustc", "--version"], text=True).strip(),
        "kernel": platform.release(), "mode": "persistent", "auto_sync_interval_seconds": 1,
        "fixture": "seed files plus one UTF-8 report and a 256-byte binary sequence with UTF-8 suffix",
        "report_bytes": len(draft), "note_bytes": len(note_bytes),
        "observed_auto_sync_wait_seconds": auto_wait, "explicit_sync_command_seconds": sync_seconds,
        "snapshot_command_seconds": snapshot_seconds, "snapshot_content_bytes": state["snapshot_bytes"],
        "trash_content_bytes_before_purge": state["trash_bytes"],
        "backend_files_bytes_before_purge": before_purge, "backend_files_bytes_after_unmount": after_unmount,
        "report_recovered_sha256": hashlib.sha256(draft).hexdigest(), "note_recovered_sha256": note_hash,
        "comparison_scope": "single small sample; no general performance claim",
    }
    (ROOT / "artifacts/showcase-metrics.json").write_text(json.dumps(metrics, ensure_ascii=False, indent=2) + "\n")
    print("ALL PASS：自动保存、异常恢复、快照差异、两种删除保护、恢复和清理均验证。", flush=True)
finally:
    h.close()
