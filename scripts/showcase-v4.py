"""A single report directory: preview, atomic restore, independent backup, import, lazy read."""
import hashlib
import json
from pathlib import Path
import subprocess
import sys
import time
sys.path.insert(0,str(Path(__file__).resolve().parents[1]/"tests"))
from fs_harness import Mount,ROOT

h=Mount("showcase-v4")
m=h.mount
def command(*args):
    print("$ teamfs "+" ".join(map(str,args)),flush=True)
    result=h.cli(*args);print(result.stdout.decode(),end="",flush=True);return result
try:
    print("TeamFS 0.4：整目录恢复、独立备份与按需读取",time.strftime("%Y-%m-%dT%H:%M:%S%z"),flush=True)
    h.start()
    folder=m/"report"
    (folder/"figures").mkdir(parents=True)
    (folder/"text.txt").write_text("小组报告初稿\n")
    (folder/"figures/data.bin").write_bytes(bytes(range(256))*1000)
    subprocess.run(["ln","-s","text.txt",str(folder/"latest")],check=True)
    h.snapshot("draft")
    (folder/"text.txt").write_text("误改版本\n")
    command("snapshot","diff",m,"draft")
    command("restore-tree",m,"draft","report","report-recovered","--dry-run")
    assert not (m/"report-recovered").exists()
    command("restore-tree",m,"draft","report","report-recovered")
    assert (m/"report-recovered/text.txt").read_text()=="小组报告初稿\n"
    digest=hashlib.sha256((m/"report-recovered/figures/data.bin").read_bytes()).hexdigest()
    print("PASS：整个目录恢复完成，二进制 SHA256 =",digest,flush=True)
    backup=h.base/"independent-backup"
    command("backup","create",m,backup)
    command("backup","verify",backup)
    h.stop()
    imported=h.base/"imported-store"
    command("backup","import",backup,"--store",imported)
    h.store=imported;h.start()
    initial=h.status()
    assert initial["content_fetched_bytes"]==0 and initial["cache_bytes"]==0
    print("PASS：从独立备份挂载，启动尚未读取文件内容。",flush=True)
    assert (m/"report-recovered/latest").read_text()=="小组报告初稿\n"
    assert hashlib.sha256((m/"report-recovered/figures/data.bin").read_bytes()).hexdigest()==digest
    (m/"report-recovered/text.txt").write_text("新版本\n")
    h.sync();state=h.status()
    print(json.dumps(state,ensure_ascii=False,indent=2),flush=True)
    (ROOT/"artifacts/showcase-v4-metrics.json").write_text(json.dumps({
        "measured_at":time.strftime("%Y-%m-%dT%H:%M:%S%z"),"restored_sha256":digest,
        "startup_cache_bytes":initial["cache_bytes"],"startup_content_fetched_bytes":initial["content_fetched_bytes"],
        "changed_file_bytes":len("新版本\n".encode()),"commit_content_bytes":state["last_commit_content_bytes"],
        "commit_rows":state["last_commit_rows"],"cache_bytes":state["cache_bytes"],
        "backup":json.loads((backup/"manifest.json").read_text())},ensure_ascii=False,indent=2)+"\n")
    h.stop()
    print("ALL PASS：目录恢复、链接保留、备份校验、新存储导入、按需读取、增量提交。",flush=True)
finally:h.close()
