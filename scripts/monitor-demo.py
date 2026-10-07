"""An isolated live playground and monitor, retained until Ctrl+C / SIGTERM."""
import argparse
import json
from pathlib import Path
import signal
import subprocess
import sys
import time
sys.path.insert(0,str(Path(__file__).resolve().parents[1]/"tests"))
from fs_harness import Mount,ROOT

parser=argparse.ArgumentParser(description="启动独立 TeamFS 实时监控体验；停止时清理自己的演示目录")
parser.add_argument("--port",type=int,default=8766)
args=parser.parse_args()
h=Mount("monitor-demo" if args.port==8766 else f"monitor-demo-{args.port}")
server=None
def stop(*_):raise KeyboardInterrupt()
signal.signal(signal.SIGTERM,stop)
try:
    h.start("--auto-sync",2,trace=False)
    server=subprocess.Popen(["python3",str(ROOT/"scripts/monitor.py"),"--mount",str(h.mount),"--port",str(args.port)])
    report=h.mount/"notes/monitor-report.txt"
    report.write_text("实时监控演示报告\n")
    h.snapshot("before-edit")
    report.write_text("修改后的报告\n")
    assert report.read_text()=="修改后的报告\n"
    target=report.with_name("report-renamed.txt")
    report.rename(target)
    note=h.mount/"notes/deleted-note.txt"
    note.write_text("演示误删\n");note.unlink()
    h.cli("restore",h.mount,"before-edit","notes/monitor-report.txt","notes/recovered.txt")
    # A deliberate missing path demonstrates a real failed lookup.
    try:(h.mount/"missing-example.txt").read_bytes()
    except FileNotFoundError:pass
    h.sync()
    info={"mountpoint":str(h.mount),"store":str(h.store),"url":f"http://127.0.0.1:{args.port}",
          "note":"独立演示目录；Ctrl+C 或终止该脚本时卸载并清理，不操作已有挂载。"}
    (ROOT/("artifacts/monitor-demo-session.json" if args.port==8766 else f"artifacts/monitor-demo-session-{args.port}.json")).write_text(json.dumps(info,ensure_ascii=False,indent=2)+"\n")
    print(json.dumps(info,ensure_ascii=False,indent=2),flush=True)
    print("现在可在另一个 Ubuntu 终端操作上面的挂载点，观察页面更新。",flush=True)
    while server.poll() is None:time.sleep(.5)
    if server.returncode:raise SystemExit(server.returncode)
except KeyboardInterrupt:pass
finally:
    if server is not None and server.poll() is None:
        server.terminate()
        try:server.wait(timeout=4)
        except subprocess.TimeoutExpired:server.kill();server.wait()
    if h.process is not None and h.process.poll() is None:
        h.stop()
    h.close()
