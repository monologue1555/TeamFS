"""Structured events and the actual read-only HTTP monitor over an isolated FUSE mount."""
import errno
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import time
from urllib.request import Request,build_opener,ProxyHandler
from urllib.error import HTTPError
from fs_harness import Mount,ROOT,wait_for,fails

h=Mount("v5")
urlopen=build_opener(ProxyHandler({})).open
m=h.mount
server=None
count=0
def passed(text):
    global count
    count+=1
    print(f"PASS {count:02}: {text}",flush=True)
def log():
    return json.loads((m/".teamfs/events.json").read_bytes())
def api(after=0,session=""):
    with urlopen(url+f"/api/monitor?after={after}&session={session}",timeout=4) as response:
        return json.load(response)
def rejected(path,code,method="GET",headers=None):
    try:
        urlopen(Request(url+path,method=method,headers=headers or {}),timeout=4)
    except HTTPError as e:assert e.code==code,(e.code,code)
    else:raise AssertionError("request was not refused")
try:
    print("TeamFS v0.5 structured logging / live monitor",time.strftime("%Y-%m-%dT%H:%M:%S%z"),flush=True)
    h.start("--auto-sync",1)
    secret=b"PRIVATE_FILE_CONTENT_NOT_FOR_LOGS_5389"
    raw=b"note-\xff\n"
    file=m/os.fsdecode(raw)
    file.write_bytes(secret)
    assert file.read_bytes()==secret
    renamed=m/"renamed.txt"
    file.rename(renamed)
    fails({errno.EFBIG},lambda:os.truncate(renamed,17*1024*1024))
    events=log()["events"]
    writes=[e for e in events if e["operation"]=="write" and e["path_bytes"]==list(raw)]
    assert writes and writes[-1]["actual_bytes"]==len(secret)
    assert writes[-1]["caller"]["pid"]==os.getpid() and writes[-1]["caller"]["uid"]==os.getuid()
    assert any(e["operation"]=="read" and e["actual_bytes"]==len(secret) for e in events)
    move=next(e for e in events if e["operation"]=="rename")
    assert move["path_bytes"]==list(raw) and move["destination_bytes"]==list(b"renamed.txt")
    assert "\n" not in move["path_display"]
    failure=next(e for e in events if e["operation"]=="setattr" and e["result"]=="error")
    assert failure["errno"]==errno.EFBIG and failure["duration_us"]>=0
    assert secret not in json.dumps(events).encode()
    assert all(events[i]["seq"]<events[i+1]["seq"] for i in range(len(events)-1))
    passed("real callbacks record PID/UID, raw names, paths, rename destination, actual bytes, duration and errno without file contents")

    wait_for(lambda:any(e["operation"]=="commit" and e["arguments"]["trigger"]=="auto_sync" for e in log()["events"]))
    h.snapshot("checkpoint")
    fd=os.open(m/".teamfs/control",os.O_WRONLY)
    os.write(fd,b'{"operation":"snapshot_create","name":"once"}')
    os.fsync(fd);os.fsync(fd);os.close(fd)
    assert len([e for e in log()["events"] if e["operation"]=="snapshot_create" and e["arguments"]["name"]=="once"])==1
    h.sync()
    before=h.status()
    seq=log()["summary"]["last_seq"]
    for _ in range(8):
        h.status();log()
    assert log()["summary"]["last_seq"]==seq
    assert h.status()["read_calls"]==before["read_calls"] and not h.status()["dirty"]
    passed("automatic/explicit commits and management commands logged; repeated control fsync stays idempotent; monitoring reads do not log themselves")

    fd=os.open(m/".teamfs/events.json",os.O_RDONLY)
    first=os.read(fd,11)
    (m/"new-after-open").write_bytes(b"new")
    parts=first
    while True:
        data=os.read(fd,29)
        if not data:break
        parts+=data
    os.close(fd)
    frozen=json.loads(parts)
    assert all(e["path_display"]!="new-after-open" for e in frozen["events"])
    assert any(e["path_display"]=="new-after-open" for e in log()["events"])
    filtered=json.loads(h.cli("logs",m,"--json","--errors","--path","renamed").stdout)
    assert filtered["events"] and all(e["result"]=="error" for e in filtered["events"])
    writes=json.loads(h.cli("logs",m,"--json","--operation","write").stdout)["events"]
    assert writes and all(e["operation"]=="write" for e in writes)
    fails({errno.EACCES,errno.EROFS},lambda:(m/".teamfs/events.json").write_bytes(b"bad"))
    passed("event JSON is immutable per open, read-only, refreshes on reopen, and CLI filters match actual records")

    db=sqlite3.connect(h.store/"state.sqlite3")
    db.execute("CREATE TRIGGER audit_failure BEFORE INSERT ON nodes BEGIN SELECT RAISE(ABORT,'audit injected sync failure'); END")
    db.commit()
    renamed.write_bytes(b"not synced")
    h.cli("sync",m,success=False)
    errors=[e for e in log()["events"] if e["operation"]=="commit" and e["result"]=="error"]
    assert "audit injected sync failure" in errors[-1]["arguments"]["storage_error"]
    db.execute("DROP TRIGGER audit_failure");db.commit();db.close();h.sync()
    wait_for(lambda:h.status()["audit"]["file"]["written"]>0)
    log_file=h.store/"logs/events.jsonl"
    wait_for(lambda:log_file.exists() and log_file.stat().st_size>0)
    parsed=[json.loads(line) for line in log_file.read_text().splitlines()]
    assert any(e["operation"]=="write" for e in parsed)
    passed("SQLite failure has structured error context; asynchronous JSONL file contains real events")

    server=subprocess.Popen(["python3",str(ROOT/"scripts/monitor.py"),"--mount",str(m),"--port","0"],
                            stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True)
    url=server.stdout.readline().strip().split("http://",1)
    assert len(url)==2,url
    url="http://"+url[1]
    wait_for(lambda:api()["connected"])
    value=api()
    assert value["status"]["filesystem"]=="TeamFS" and value["events"]
    assert "store_path_bytes" not in value["status"]
    session=value["stream"]["session_id"]
    cursor=value["cursor"]
    assert all(e["seq"]>cursor for e in api(cursor,session)["events"])
    rejected("/api/monitor",405,"POST")
    rejected("/api/monitor",403,headers={"Origin":"https://example.com"})
    rejected("/api/monitor",403,headers={"Host":"example.com"})
    rejected("/../README.md",404)
    rejected("/api/monitor?after=-1",400)
    with urlopen(url+"/",timeout=3) as response:
        assert "真实挂载".encode() in response.read()
    passed("localhost server samples real FUSE data, supports cursor queries, and exposes only read-only same-origin routes")

    h.stop()
    wait_for(lambda:not api()["connected"])
    assert api()["last_good_at_ms"] is not None
    h.start()
    wait_for(lambda:api()["connected"] and api()["stream"]["session_id"]!=session)
    assert api(cursor,session)["reset"]
    h.stop()
    passed("disconnect reports stale data explicitly; remount switches session and resets event cursors")

    bad=h.base/"blocked-log"
    bad.mkdir()
    h.start("--audit-log",bad)
    wait_for(lambda:h.status()["audit"]["file"]["last_error"] is not None)
    (m/"still-works").write_bytes(b"business succeeds")
    h.sync()
    assert (m/"still-works").read_bytes()==b"business succeeds"
    assert h.status()["audit"]["file"]["dropped"]>0
    bad.rmdir()
    (m/"still-works").write_bytes(b"retry logging")
    h.sync()
    wait_for(lambda:h.status()["audit"]["file"]["last_error"] is None and bad.exists())
    h.stop()
    passed("unwritable log does not fail business writes or sync; repaired logging resumes automatically")

    rotating=h.base/"rotating.jsonl"
    rotating.write_bytes(b'{}\n'*(8*1024*1024//3+1))
    Path(str(rotating)+".1").write_text('{"old":1}\n')
    Path(str(rotating)+".2").write_text('{"old":2}\n')
    h.start("--audit-log",rotating)
    wait_for(lambda:h.status()["audit"]["file"]["written"]>0)
    assert Path(str(rotating)+".1").stat().st_size>8*1024*1024
    assert Path(str(rotating)+".2").read_text()=='{"old":1}\n'
    assert all("operation" in json.loads(line) for line in rotating.read_text().splitlines())
    h.stop()
    h.start("--memory")
    (m/"memory-log").touch()
    assert h.status()["audit"]["file"] is None and any(e["operation"]=="create" for e in log()["events"])
    h.stop()
    h.cli("mount",m,"--store",h.store,"--audit-log",m/"recursive.jsonl",success=False)
    assert not (m/"recursive.jsonl").exists()
    passed("file logs rotate independently, memory mode retains live events, and recursive log paths are refused")
    (ROOT/"artifacts/events-example.json").write_text(json.dumps({"session_id":session,"events":events},ensure_ascii=False,indent=2)+"\n")
    print(f"ALL PASS: {count} v5 mounted/HTTP scenarios",flush=True)
finally:
    if server is not None:
        server.terminate()
        try:server.communicate(timeout=5)
        except subprocess.TimeoutExpired:server.kill();server.communicate()
    h.close()
