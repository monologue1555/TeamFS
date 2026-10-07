"""Actual applications and syscall contracts, in both supported I/O modes."""
import contextlib
import errno
import fcntl
import hashlib
import json
import mmap
import os
from pathlib import Path
import shutil
import sqlite3
import stat
import subprocess
import sys
from fs_harness import Mount, fails, ROOT
from reporting import Report, EnvironmentSkip, Unsupported

report = Report("compatibility-v6")


def tool(name, *args, cwd=None):
    if not shutil.which(name):
        raise EnvironmentSkip(f"missing executable: {name}")
    p = subprocess.run([name, *map(str, args)], cwd=cwd, capture_output=True, timeout=30)
    assert p.returncode == 0, (name, args, p.returncode, p.stdout, p.stderr)
    return p.stdout


def fingerprint(root):
    result = {}
    for base, dirs, files in os.walk(root):
        for name in dirs+files:
            p = Path(base)/name
            s = p.lstat()
            value = os.readlink(p) if p.is_symlink() else hashlib.sha256(p.read_bytes()).hexdigest() if p.is_file() else None
            result[os.fsencode(p.relative_to(root)).hex()] = (stat.S_IFMT(s.st_mode), s.st_mode & 0o777, value)
    return result


for mode in ["direct", "cached"]:
    h = Mount("compat-"+mode)
    try:
        h.start(*(["--cached-io"] if mode == "cached" else []), trace=False)
        m = h.mount
        report.context[mode] = h.status()["version"]

        def git_workflow():
            p = m/"git-project"; p.mkdir()
            def git(*args):
                return tool("git", "-c", "commit.gpgsign=false", "-c", "core.autocrlf=false", "-c", "core.hooksPath=/dev/null", *args, cwd=p)
            git("init", "-b", "main")
            git("config", "user.name", "TeamFS Test")
            git("config", "user.email", "test@example.invalid")
            (p/"报告.txt").write_text("initial\n")
            git("add", "."); git("commit", "-m", "initial")
            git("checkout", "-b", "feature")
            (p/"报告.txt").write_text("changed\n")
            git("commit", "-am", "change")
            git("checkout", "main")
            assert (p/"报告.txt").read_text() == "initial\n"
            git("checkout", "feature")
            assert (p/"报告.txt").read_text() == "changed\n"
            assert not git("status", "--porcelain").strip()
            git("fsck", "--full")
            return "git init/add/commit/checkout/status/fsck completed"
        report.case(mode+" / Git workflow", git_workflow)

        def rsync_workflow():
            src = h.base/"rsync-source"; src.mkdir()
            (src/"子目录").mkdir(); (src/"子目录/empty").touch()
            (src/"子目录"/os.fsdecode(b"raw-\xff\n")).write_bytes(bytes(range(256)))
            (src/"正文").write_bytes(b"first")
            os.symlink("正文", src/"relative")
            dst = m/"rsync-target"; dst.mkdir()
            tool("rsync", "-a", str(src)+"/", str(dst)+"/")
            assert fingerprint(src) == fingerprint(dst)
            (src/"正文").write_bytes(b"second"); (src/"子目录/empty").unlink()
            tool("rsync", "-ac", "--delete", str(src)+"/", str(dst)+"/")
            assert fingerprint(src) == fingerprint(dst)
            return "recursive update/delete with binary, raw names, empty file and symlink"
        report.case(mode+" / rsync round trip", rsync_workflow)

        def editors():
            f = m/"vim.txt"; f.write_text("draft\n")
            for i in range(5):
                tool("vim", "-Nu", "NONE", "-i", "NONE", "-n", "-es", f, "-c", f"%s/.*/saved-{i}/", "-c", "wq")
                assert f.read_text() == f"saved-{i}\n"
        report.case(mode+" / repeated Vim saves", editors)

        def atomic_replace():
            f = m/"atomic"; f.write_bytes(b"A"*4096)
            code = """import sys,pathlib
p=pathlib.Path(sys.argv[1])
for _ in range(800):
 data=p.read_bytes()
 assert data in (b'A'*4096,b'B'*4096),len(data)
"""
            child = subprocess.Popen([sys.executable, "-c", code, str(f)])
            try:
                for i in range(40):
                    tmp = m/"atomic.tmp"
                    with tmp.open("wb") as out:
                        out.write((b"A" if i%2 else b"B")*4096); out.flush(); os.fsync(out.fileno())
                    os.replace(tmp, f)
                assert child.wait(timeout=20) == 0
            finally:
                if child.poll() is None: child.kill(); child.wait()
            assert not (m/"atomic.tmp").exists()
        report.case(mode+" / atomic replace under concurrent reads", atomic_replace)

        def record_locks():
            f = m/"locked"; f.write_bytes(b"x")
            with f.open("r+b") as held:
                fcntl.lockf(held, fcntl.LOCK_EX|fcntl.LOCK_NB)
                tool(sys.executable, "-c", "import fcntl,sys\nf=open(sys.argv[1],'r+b')\ntry: fcntl.lockf(f,fcntl.LOCK_EX|fcntl.LOCK_NB)\nexcept BlockingIOError: sys.exit(0)\nsys.exit(1)", f)
            with f.open("r+b") as free: fcntl.lockf(free, fcntl.LOCK_EX|fcntl.LOCK_NB)
        report.case(mode+" / local POSIX record locks", record_locks)

        def sqlite_app(journal="PERSIST", explicit=False):
            dbpath = m/("application-"+journal+str(explicit)+".sqlite")
            with contextlib.closing(sqlite3.connect(dbpath)) as db:
                assert db.execute("PRAGMA journal_mode="+journal).fetchone()[0] == journal.lower()
                db.execute("PRAGMA synchronous=FULL")
                db.execute("CREATE TABLE records(id PRIMARY KEY,data BLOB)")
                for i in range(30): db.execute("INSERT INTO records VALUES(?,?)", (i,bytes([i])*100))
                db.commit()
                db.execute("UPDATE records SET data=X'0000'"); db.rollback()
            if explicit: h.sync()
            # PERSIST tests application fsync; DELETE requires the documented TeamFS sync.
            h.stop(kill=True); h.start(*(["--cached-io"] if mode=="cached" else []), trace=False)
            with contextlib.closing(sqlite3.connect(dbpath)) as db:
                assert db.execute("PRAGMA integrity_check").fetchone()[0] == "ok"
                recovered=db.execute("SELECT data FROM records ORDER BY id").fetchall()
                expected=[(bytes([i])*100,) for i in range(30)]
                if journal=="DELETE" and not explicit:
                    report.context.setdefault("sqlite_delete_boundary",{})[mode]={"committed_rows":30,"recovered_rows":len(recovered),"matches_committed":recovered==expected}
                    if recovered!=expected: raise Unsupported(f"Reproduced DELETE-mode durability boundary: committed 30 rows, recovered {len(recovered)}. Use explicit TeamFS sync or validated PERSIST mode.")
                assert recovered==expected
            return f"SQLite {journal}, explicit TeamFS sync={explicit}; WAL is not claimed"
        report.case(mode+" / SQLite PERSIST commit, rollback and daemon crash", sqlite_app)
        report.case(mode+" / SQLite DELETE plus explicit sync", lambda:sqlite_app("DELETE",True))
        report.case(mode+" / SQLite DELETE without explicit sync", lambda:sqlite_app("DELETE",False))

        def errors_and_handles():
            f=m/"errors";f.write_bytes(b"old")
            fd=os.open(f,os.O_RDWR);f.unlink();assert os.pread(fd,3,0)==b"old"
            os.pwrite(fd,b"NEW",0);assert os.pread(fd,3,0)==b"NEW";os.close(fd)
            d=m/"notempty";d.mkdir();(d/"child").touch()
            fails({errno.ENOTEMPTY},lambda:d.rmdir())
            fails({errno.EEXIST},lambda:os.open(d/"child",os.O_CREAT|os.O_EXCL|os.O_WRONLY))
            fails({errno.ENOTDIR},lambda:(d/"child/x").read_bytes())
            assert (d/"child").exists()
            (d/"child").chmod(0)
            fails({errno.EACCES},lambda:(d/"child").read_bytes())
            (d/"child").chmod(0o600)
        report.case(mode+" / error contracts and unlinked open handles", errors_and_handles)

        def mapping():
            if mode!="cached": raise Unsupported("default direct_io intentionally does not offer shared mmap")
            f=m/"mapping";f.write_bytes(b"A"*8192)
            with f.open("r+b") as stream:
                with mmap.mmap(stream.fileno(),8192) as mm:
                    mm[17:21]=b"test";mm.flush();os.fsync(stream.fileno())
            h.snapshot("mapped")
            assert (m/".teamfs/snapshots/mapped/mapping").read_bytes()[17:21]==b"test"
        report.case(mode+" / mmap then snapshot", mapping)

        def unsupported(which):
            f=m/("probe-"+which);f.write_bytes(b"probe")
            try:
                if which=="hardlink": os.link(f,m/"probe-link")
                else: os.setxattr(f,b"user.teamfs",b"x")
            except OSError as e:
                assert e.errno in (errno.ENOSYS,errno.EOPNOTSUPP,errno.EPERM) if which=="hardlink" else e.errno in (errno.ENOSYS,errno.EOPNOTSUPP),e
                raise Unsupported(f"{which} is explicitly unsupported, errno={e.errno}")
            raise AssertionError("support changed: add full behavioral tests before claiming this capability")
        report.case(mode+" / hardlink capability",lambda:unsupported("hardlink"))
        report.case(mode+" / xattr capability",lambda:unsupported("xattr"))
        report.case(mode+" / clean unmount", lambda: (h.sync(), h.stop()))
    finally:
        h.close()

(ROOT/"artifacts/sqlite-boundary-v6.json").write_text(json.dumps(report.context.get("sqlite_delete_boundary",{}),indent=2)+"\n")
report.finish()
