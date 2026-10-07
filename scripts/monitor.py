"""Read-only localhost bridge. Samples only TeamFS's virtual status and event files."""
import argparse
import json
from pathlib import Path
import signal
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs, urlsplit

ROOT=Path(__file__).resolve().parents[1]
ASSETS={"/":("monitor.html","text/html; charset=utf-8"),
        "/monitor.js":("monitor.js","text/javascript; charset=utf-8"),
        "/monitor.css":("monitor.css","text/css; charset=utf-8")}

class Samples:
    def __init__(self,mount):
        self.mount=mount
        self.lock=threading.Lock()
        self.stop=threading.Event()
        self.current={"connected":False,"error":"等待首次采样","sampled_at_ms":None}
        self.last_good=None
        self.worker=threading.Thread(target=self.run,daemon=True,name="teamfs-monitor-reader")
        self.worker.start()

    def run(self):
        while not self.stop.is_set():
            try:
                status=json.loads((self.mount/".teamfs/status.json").read_bytes())
                if status.get("filesystem")!="TeamFS":
                    raise ValueError("目标不是 TeamFS 挂载点")
                events=json.loads((self.mount/".teamfs/events.json").read_bytes())
                if status.get("audit",{}).get("session_id")!=events["summary"]["session_id"]:
                    raise ValueError("挂载会话正在切换，等待下一次采样")
                # Internal database path is not needed by a monitoring UI.
                status.pop("store_path_bytes",None)
                value={"connected":True,"error":None,"sampled_at_ms":int(time.time()*1000),
                       "status":status,"stream":events["summary"],"events":events["events"]}
                with self.lock:
                    self.current=value
                    self.last_good=value
            except (OSError,ValueError,KeyError,TypeError) as error:
                with self.lock:
                    self.current={"connected":False,"error":"无法读取挂载数据："+str(error),
                                  "sampled_at_ms":int(time.time()*1000)}
            self.stop.wait(1)

    def response(self,after,session):
        with self.lock:
            value=dict(self.current)
            last=self.last_good
        value["mountpoint"]=str(self.mount)
        value["server_time_ms"]=int(time.time()*1000)
        if value["connected"] and value["server_time_ms"]-value["sampled_at_ms"]>4000:
            value={"connected":False,"error":"采样超时，文件系统可能正在等待存储或请求处理",
                   "sampled_at_ms":value["sampled_at_ms"],"mountpoint":str(self.mount),
                   "server_time_ms":value["server_time_ms"]}
        if not value["connected"]:
            value["last_good_at_ms"]=last["sampled_at_ms"] if last else None
            return value
        reset=session!=value["stream"]["session_id"]
        start=0 if reset else after
        events=[e for e in value["events"] if e["seq"]>start][:500]
        value["events"]=events
        value["reset"]=reset
        first=value["stream"]["first_seq"]
        value["gap"]=max(0,(first or 1)-start-1) if not reset else value["stream"]["evicted"]
        value["cursor"]=events[-1]["seq"] if events else max(start,value["stream"]["last_seq"])
        return value

class Handler(BaseHTTPRequestHandler):
    def log_message(self,*args):
        pass
    def respond(self,status,content,kind="application/json; charset=utf-8"):
        if not isinstance(content,bytes):
            content=json.dumps(content,ensure_ascii=False).encode()
        self.send_response(status)
        self.send_header("Content-Type",kind)
        self.send_header("Content-Length",str(len(content)))
        self.send_header("Cache-Control","no-store")
        self.send_header("X-Content-Type-Options","nosniff")
        self.send_header("Content-Security-Policy","default-src 'self'; script-src 'self'; style-src 'self'; frame-ancestors 'none'")
        self.end_headers()
        try:self.wfile.write(content)
        except (BrokenPipeError,ConnectionResetError):pass
    def local_request(self):
        port=self.server.server_address[1]
        hosts={f"127.0.0.1:{port}",f"localhost:{port}"}
        if self.headers.get("Host","").lower() not in hosts:return False
        origin=self.headers.get("Origin")
        return not origin or origin.lower() in {f"http://{h}" for h in hosts}
    def do_GET(self):
        if not self.local_request():
            self.respond(403,{"error":"仅接受本机同源访问"});return
        parsed=urlsplit(self.path)
        if parsed.path=="/api/monitor":
            query=parse_qs(parsed.query)
            try:
                after=int(query.get("after",["0"])[0])
                if after<0:raise ValueError()
                session=query.get("session",[""])[0]
            except (ValueError,IndexError):
                self.respond(400,{"error":"无效的日志游标"});return
            self.respond(200,self.server.samples.response(after,session))
        elif parsed.path in ASSETS:
            name,kind=ASSETS[parsed.path]
            self.respond(200,(ROOT/"demo"/name).read_bytes(),kind)
        else:self.respond(404,{"error":"not found"})
    def do_POST(self):self.respond(405,{"error":"监控接口只读"})
    do_PUT=do_POST
    do_DELETE=do_POST

def main():
    parser=argparse.ArgumentParser(description="TeamFS 只读真实监控（仅监听 127.0.0.1）")
    parser.add_argument("--mount",type=Path,required=True)
    parser.add_argument("--port",type=int,default=8766)
    args=parser.parse_args()
    # Resolve once on startup. API callers cannot change the monitored path.
    mount=args.mount.absolute()
    server=ThreadingHTTPServer(("127.0.0.1",args.port),Handler)
    server.daemon_threads=True
    server.samples=Samples(mount)
    signal.signal(signal.SIGTERM,lambda *_:threading.Thread(target=server.shutdown,daemon=True).start())
    print(f"TeamFS 真实监控：http://127.0.0.1:{server.server_address[1]}",flush=True)
    print(f"只读采样：{mount}；Ctrl+C 停止监控，不卸载文件系统。",flush=True)
    try:server.serve_forever(poll_interval=.25)
    except KeyboardInterrupt:pass
    finally:
        server.samples.stop.set()
        server.server_close()

if __name__=="__main__":main()
