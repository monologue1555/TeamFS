"""Seeded differential operations against a native directory, plus durable checkpoints.
Replay: python3 tests/randomized.py --seed 1555 --steps 180 --mode direct
The saved JSONL is the exact generated sequence (data and raw paths included).
"""
import argparse
import errno
import json
import os
from pathlib import Path
import random
import shutil
import stat
from fs_harness import Mount, ROOT
from reporting import Report


def capture(root, readonly=False):
    out={}
    for base, dirs, files in os.walk(root):
        for name in dirs+files:
            p=Path(base)/name;s=p.lstat();key=os.fsencode(p.relative_to(root)).hex()
            out[key]={"mode":(0o555 if stat.S_ISDIR(s.st_mode) else 0o444) if readonly else stat.S_IMODE(s.st_mode),"kind":stat.S_IFMT(s.st_mode),
                "data":os.fsencode(os.readlink(p)).hex() if p.is_symlink() else p.read_bytes().hex() if p.is_file() else ""}
    return out


def reset_reference(root, state):
    assert root.name=="reference" and root.parent.name.startswith("teamfs-random-")
    shutil.rmtree(root);root.mkdir()
    for key,value in sorted(state.items(),key=lambda kv:(bytes.fromhex(kv[0]).count(b"/"),kv[0])):
        p=root/os.fsdecode(bytes.fromhex(key));data=bytes.fromhex(value["data"])
        if value["kind"]==stat.S_IFDIR:p.mkdir()
        elif value["kind"]==stat.S_IFLNK:os.symlink(os.fsdecode(data),p)
        else:p.write_bytes(data)
    for key,value in sorted(state.items(),key=lambda kv:bytes.fromhex(kv[0]).count(b"/"),reverse=True):
        p=root/os.fsdecode(bytes.fromhex(key))
        if not p.is_symlink():p.chmod(value["mode"])


def apply(root, op):
    p=root/os.fsdecode(bytes.fromhex(op["path"]))
    q=root/os.fsdecode(bytes.fromhex(op["target"]))
    try:
        if op["op"]=="create":
            fd=os.open(p,os.O_CREAT|os.O_EXCL|os.O_WRONLY,0o600)
            try:os.write(fd,bytes.fromhex(op["data"]))
            finally:os.close(fd)
        elif op["op"] in ("write","append"):
            fd=os.open(p,os.O_WRONLY|(os.O_APPEND if op["op"]=="append" else 0))
            try:
                if op["op"]=="write":os.pwrite(fd,bytes.fromhex(op["data"]),op["offset"])
                else:os.write(fd,bytes.fromhex(op["data"]))
            finally:os.close(fd)
        elif op["op"]=="truncate":os.truncate(p,op["offset"])
        elif op["op"]=="chmod":p.chmod(op["mode"])
        elif op["op"]=="rename":os.rename(p,q)
        elif op["op"]=="unlink":p.unlink()
        elif op["op"]=="mkdir":p.mkdir(mode=0o700)
        elif op["op"]=="rmdir":p.rmdir()
        return 0
    except OSError as e:
        # POSIX permits either code when replacing a nonempty directory.
        return errno.ENOTEMPTY if e.errno==errno.EEXIST and op["op"]=="rename" else e.errno


def run(seed,steps,mode):
    h=Mount(f"random-{seed}-{mode}")
    trace_path=ROOT/"artifacts"/f"random-{seed}-{mode}.jsonl"
    log=[]
    rng=random.Random(seed)
    try:
        options=["--cached-io"] if mode=="cached" else []
        h.start(*options,trace=False)
        live=h.mount/"work";live.mkdir()
        ref=h.base/"reference";ref.mkdir()
        for root in (live,ref):
            (root/"a").mkdir();(root/"b").mkdir()
            (root/"a/start").write_bytes(b"start");(root/"a/start").chmod(0o600)
        h.sync();committed=capture(ref);snap=None;crashes=0
        paths=[d+b"/"+n for d in (b"a",b"b") for n in (b"start",b"one",b"two",b"raw-\xff\n","中文".encode(),b"dir",b"dir/child")]
        for step in range(steps):
            op={"step":step,"op":rng.choice(["create","write","append","truncate","chmod","rename","unlink","mkdir","rmdir"]),
                "path":rng.choice(paths).hex(),"target":rng.choice(paths).hex(),
                "offset":rng.randrange(0,400),"data":rng.randbytes(rng.randrange(0,100)).hex(),"mode":rng.choice([0o600,0o640,0o644,0o700])}
            expected=apply(ref,op);actual=apply(live,op);op.update(expected_errno=expected,actual_errno=actual);log.append(op)
            assert actual==expected,(step,op)
            assert capture(live)==capture(ref),(step,op,"tree mismatch")
            if step%37==0:
                if snap is not None:h.cli("snapshot","delete",h.mount,"random-checkpoint")
                h.snapshot("random-checkpoint");snap=capture(ref,readonly=True);committed=capture(ref)
                log.append({"step":step,"op":"snapshot_commit"})
            elif step%17==0:
                h.sync();committed=capture(ref);log.append({"step":step,"op":"sync"})
            if step%29==28:
                h.stop(kill=True);h.start(*options,trace=False);reset_reference(ref,committed);crashes+=1
                log.append({"step":step,"op":"kill_remount"})
                assert capture(live)==committed,(step,"durable checkpoint mismatch")
            if snap is not None:
                assert capture(h.mount/".teamfs/snapshots/random-checkpoint/work")==snap,(step,"snapshot changed")
        h.sync();h.stop()
        return {"seed":seed,"steps":steps,"mode":mode,"crashes":crashes,"trace":trace_path.name}
    finally:
        trace_path.write_text("".join(json.dumps(x,ensure_ascii=False)+"\n" for x in log))
        h.close()


if __name__=="__main__":
    parser=argparse.ArgumentParser()
    parser.add_argument("--seed",type=int,action="append")
    parser.add_argument("--steps",type=int,default=180)
    parser.add_argument("--mode",choices=["direct","cached","both"],default="both")
    args=parser.parse_args()
    if args.steps<1:parser.error("steps must be positive")
    report=Report("randomized-v6")
    report.context={"oracle":"native directory syscall outcomes + independent checkpoint copies","notes":"mtime/atime/inode numbers excluded; raw paths and bytes compared"}
    for mode in (["direct","cached"] if args.mode=="both" else [args.mode]):
        for seed in args.seed or [1555,20261007,42]:
            report.case(f"{mode} seed={seed} steps={args.steps}",lambda:run(seed,args.steps,mode))
    report.finish()
