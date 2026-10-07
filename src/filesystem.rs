//! FUSE 适配层：回调统一进入 Service，持久化和管理请求共享同一份状态。
use crate::model::{Attr, FsResult, Kind};
use crate::service::Service;
use fuse::{
    FileAttr, FileType, Filesystem, ReplyAttr, ReplyCreate, ReplyData, ReplyDirectory, ReplyEmpty,
    ReplyEntry, ReplyOpen, ReplyStatfs, ReplyWrite, Request,
};
use serde_json::{json, Value};
use std::os::unix::ffi::OsStrExt;
use std::time::Instant;
use std::{
    ffi::OsStr,
    sync::{Arc, Mutex},
};
use time::Timespec;
const TTL: Timespec = Timespec { sec: 0, nsec: 0 };
const DIRECT_IO: u32 = 1;
pub struct TeamFs {
    service: Arc<Mutex<Service>>,
    trace: bool,
    sequence: u64,
    cached_io: bool,
}
impl TeamFs {
    pub fn new(service: Arc<Mutex<Service>>, trace: bool, cached_io: bool) -> Self {
        Self {
            service,
            trace,
            sequence: 0,
            cached_io,
        }
    }
    fn run<T: Observed>(
        &mut self,
        req: &Request,
        op: &str,
        ino: u64,
        detail: Value,
        f: impl FnOnce(&mut Service) -> FsResult<T>,
    ) -> FsResult<T> {
        let began = Instant::now();
        let result = self
            .service
            .lock()
            .map_err(|_| libc::EIO)
            .and_then(|mut s| {
                let lock_wait_us = began.elapsed().as_micros() as u64;
                let name: Option<Vec<u8>> =
                    serde_json::from_value(detail["name_bytes"].clone()).ok();
                let path = s.audit_path(ino, name.as_deref());
                let destination = detail["new_parent"].as_u64().and_then(|parent| {
                    let name: Vec<u8> =
                        serde_json::from_value(detail["new_name_bytes"].clone()).ok()?;
                    s.audit_path(parent, Some(&name))
                });
                let visible = s.audit_visible(ino, path.as_deref());
                let caller = crate::audit::Caller {
                    pid: req.pid(),
                    uid: req.uid(),
                    gid: req.gid(),
                };
                s.caller = Some(caller.clone());
                s.operation_context = Some(op.into());
                let commit_before = s.metrics.commit_total_us;
                let service_started = Instant::now();
                let result = f(&mut s);
                let service_us = service_started.elapsed().as_micros() as u64;
                let timing = crate::metrics::Timing {
                    total_us: began.elapsed().as_micros() as u64,
                    lock_wait_us,
                    service_us,
                    commit_us: s.metrics.commit_total_us.saturating_sub(commit_before),
                };
                let mut detail = detail.clone();
                detail["timing_us"] = serde_json::to_value(timing).unwrap();
                s.caller = None;
                s.operation_context = None;
                if visible && (!matches!(op, "lookup" | "getattr" | "forget") || result.is_err()) {
                    s.metrics.record(
                        &format!("fuse.{op}"),
                        path.as_deref(),
                        Some(caller.pid),
                        result.as_ref().err().copied(),
                        timing,
                    );
                    let bytes = if matches!(op, "read" | "write") {
                        Some(result.as_ref().ok().and_then(|v| v.bytes()).unwrap_or(0))
                    } else {
                        None
                    };
                    let inode = result.as_ref().ok().and_then(|v| v.inode()).unwrap_or(ino);
                    let dirty = s.dirty;
                    s.audit.record(
                        "fuse",
                        op,
                        Some(caller),
                        Some(inode),
                        path,
                        destination,
                        detail.clone(),
                        result.as_ref().err().copied(),
                        bytes,
                        timing.total_us,
                        dirty,
                    );
                }
                result
            });
        if self.trace {
            self.sequence += 1;
            let status = match &result {
                Ok(_) => "OK".into(),
                Err(e) => format!("errno={e} ({})", std::io::Error::from_raw_os_error(*e)),
            };
            eprintln!(
                "[{:04}] {:<10} ino={ino} {detail} => {status}",
                self.sequence, op
            );
        }
        result
    }
}
fn kind(k: Kind) -> FileType {
    match k {
        Kind::File => FileType::RegularFile,
        Kind::Directory => FileType::Directory,
        Kind::Symlink => FileType::Symlink,
    }
}
fn attr(a: Attr) -> FileAttr {
    FileAttr {
        ino: a.ino,
        size: a.size,
        blocks: (a.size + 511) / 512,
        atime: a.atime,
        mtime: a.mtime,
        ctime: a.ctime,
        crtime: a.crtime,
        kind: kind(a.kind),
        perm: a.mode,
        nlink: a.nlink,
        uid: a.uid,
        gid: a.gid,
        rdev: 0,
        flags: 0,
    }
}
fn empty(result: FsResult<()>, reply: ReplyEmpty) {
    match result {
        Ok(()) => reply.ok(),
        Err(e) => reply.error(e),
    }
}
impl Filesystem for TeamFs {
    fn symlink(
        &mut self,
        req: &Request,
        parent: u64,
        name: &OsStr,
        link: &std::path::Path,
        reply: ReplyEntry,
    ) {
        match self.run(
            req,
            "symlink",
            parent,
            json!({"name_bytes":name.as_bytes()}),
            |s| {
                let ino = s.symlink(
                    parent,
                    name,
                    link.as_os_str().as_bytes(),
                    req.uid(),
                    req.gid(),
                )?;
                s.remember(ino);
                s.attr(ino)
            },
        ) {
            Ok(a) => reply.entry(&TTL, &attr(a), 0),
            Err(e) => reply.error(e),
        }
    }
    fn readlink(&mut self, req: &Request, ino: u64, reply: ReplyData) {
        match self.run(req, "readlink", ino, json!({}), |s| s.readlink(ino)) {
            Ok(bytes) => reply.data(&bytes),
            Err(e) => reply.error(e),
        }
    }
    fn lookup(&mut self, req: &Request, parent: u64, name: &OsStr, reply: ReplyEntry) {
        let result = self.run(
            req,
            "lookup",
            parent,
            json!({"name_bytes":name.as_bytes()}),
            |s| {
                let ino = s.lookup(parent, name)?;
                let a = s.attr(ino)?;
                s.remember(ino);
                Ok(a)
            },
        );
        match result {
            Ok(a) => reply.entry(&TTL, &attr(a), 0),
            Err(e) => reply.error(e),
        }
    }
    fn forget(&mut self, req: &Request, ino: u64, nlookup: u64) {
        let _ = self.run(req, "forget", ino, json!({"nlookup":nlookup}), |s| {
            s.forget(ino, nlookup);
            Ok(())
        });
    }
    fn getattr(&mut self, req: &Request, ino: u64, reply: ReplyAttr) {
        match self.run(req, "getattr", ino, json!({}), |s| s.attr(ino)) {
            Ok(a) => reply.attr(&TTL, &attr(a)),
            Err(e) => reply.error(e),
        }
    }
    fn setattr(
        &mut self,
        req: &Request,
        ino: u64,
        mode: Option<u32>,
        uid: Option<u32>,
        gid: Option<u32>,
        size: Option<u64>,
        atime: Option<Timespec>,
        mtime: Option<Timespec>,
        _fh: Option<u64>,
        _crtime: Option<Timespec>,
        _chgtime: Option<Timespec>,
        _bkuptime: Option<Timespec>,
        _flags: Option<u32>,
        reply: ReplyAttr,
    ) {
        let result = self.run(
            req,
            "setattr",
            ino,
            json!({"size":size,"mode":mode,"uid":uid,"gid":gid}),
            |s| {
                if let Some(size) = size {
                    s.truncate(ino, size)?;
                }
                s.set_metadata(ino, mode, uid, gid, atime, mtime)
            },
        );
        match result {
            Ok(a) => reply.attr(&TTL, &attr(a)),
            Err(e) => reply.error(e),
        }
    }
    fn mkdir(&mut self, req: &Request, parent: u64, name: &OsStr, mode: u32, reply: ReplyEntry) {
        let result = self.run(
            req,
            "mkdir",
            parent,
            json!({"name_bytes":name.as_bytes()}),
            |s| {
                let ino = s.create(parent, name, Kind::Directory, mode, req.uid(), req.gid())?;
                let a = s.attr(ino)?;
                s.remember(ino);
                Ok(a)
            },
        );
        match result {
            Ok(a) => reply.entry(&TTL, &attr(a), 0),
            Err(e) => reply.error(e),
        }
    }
    fn mknod(
        &mut self,
        req: &Request,
        parent: u64,
        name: &OsStr,
        mode: u32,
        _rdev: u32,
        reply: ReplyEntry,
    ) {
        let result = self.run(
            req,
            "mknod",
            parent,
            json!({"name_bytes":name.as_bytes()}),
            |s| {
                if mode & libc::S_IFMT != libc::S_IFREG {
                    return Err(libc::EOPNOTSUPP);
                }
                let ino = s.create(parent, name, Kind::File, mode, req.uid(), req.gid())?;
                let a = s.attr(ino)?;
                s.remember(ino);
                Ok(a)
            },
        );
        match result {
            Ok(a) => reply.entry(&TTL, &attr(a), 0),
            Err(e) => reply.error(e),
        }
    }
    fn create(
        &mut self,
        req: &Request,
        parent: u64,
        name: &OsStr,
        mode: u32,
        flags: u32,
        reply: ReplyCreate,
    ) {
        let result = self.run(
            req,
            "create",
            parent,
            json!({"name_bytes":name.as_bytes(),"flags":flags}),
            |s| {
                if flags as i32 & libc::O_ACCMODE == 3 {
                    return Err(libc::EINVAL);
                }
                let ino = s.create(parent, name, Kind::File, mode, req.uid(), req.gid())?;
                let fh = s.open(ino, flags as i32)?;
                let a = s.attr(ino)?;
                s.remember(ino);
                Ok((a, fh))
            },
        );
        match result {
            Ok((a, fh)) => reply.created(
                &TTL,
                &attr(a),
                0,
                fh,
                if self.cached_io { 0 } else { DIRECT_IO },
            ),
            Err(e) => reply.error(e),
        }
    }
    fn open(&mut self, req: &Request, ino: u64, flags: u32, reply: ReplyOpen) {
        match self.run(req, "open", ino, json!({"flags":flags}), |s| {
            s.open(ino, flags as i32)
        }) {
            Ok(fh) => reply.opened(
                fh,
                if self.cached_io && ino < crate::service::ADMIN {
                    0
                } else {
                    DIRECT_IO
                },
            ),
            Err(e) => reply.error(e),
        }
    }
    fn read(&mut self, req: &Request, ino: u64, fh: u64, offset: i64, size: u32, reply: ReplyData) {
        match self.run(
            req,
            "read",
            ino,
            json!({"fh":fh,"offset":offset,"requested_bytes":size}),
            |s| s.read(ino, fh, offset, size),
        ) {
            Ok(data) => reply.data(&data),
            Err(e) => reply.error(e),
        }
    }
    fn write(
        &mut self,
        req: &Request,
        ino: u64,
        fh: u64,
        offset: i64,
        data: &[u8],
        _flags: u32,
        reply: ReplyWrite,
    ) {
        match self.run(
            req,
            "write",
            ino,
            json!({"fh":fh,"offset":offset,"requested_bytes":data.len()}),
            |s| s.write(ino, fh, offset, data),
        ) {
            Ok(size) => reply.written(size),
            Err(e) => reply.error(e),
        }
    }
    fn flush(&mut self, req: &Request, ino: u64, fh: u64, _lock_owner: u64, reply: ReplyEmpty) {
        empty(
            self.run(
                req,
                "flush",
                ino,
                json!({"fh":fh,"sync_point":false}),
                |s| s.check_handle(ino, fh),
            ),
            reply,
        );
    }
    fn release(
        &mut self,
        req: &Request,
        ino: u64,
        fh: u64,
        _flags: u32,
        _lock_owner: u64,
        _flush: bool,
        reply: ReplyEmpty,
    ) {
        empty(
            self.run(req, "release", ino, json!({"fh":fh}), |s| s.close(ino, fh)),
            reply,
        );
    }
    fn fsync(&mut self, req: &Request, ino: u64, fh: u64, _datasync: bool, reply: ReplyEmpty) {
        empty(
            self.run(req, "fsync", ino, json!({"fh":fh}), |s| s.fsync(ino, fh)),
            reply,
        );
    }
    fn opendir(&mut self, req: &Request, ino: u64, flags: u32, reply: ReplyOpen) {
        match self.run(req, "opendir", ino, json!({}), |s| {
            s.open_dir(ino, flags as i32)
        }) {
            Ok(fh) => reply.opened(fh, 0),
            Err(e) => reply.error(e),
        }
    }
    fn readdir(
        &mut self,
        req: &Request,
        ino: u64,
        fh: u64,
        offset: i64,
        mut reply: ReplyDirectory,
    ) {
        let result = self.run(req, "readdir", ino, json!({"fh":fh,"offset":offset}), |s| {
            if offset < 0 {
                Err(libc::EINVAL)
            } else {
                s.directory_entries(ino, fh).map(|e| e.to_vec())
            }
        });
        match result {
            Ok(entries) => {
                for (index, e) in entries.iter().enumerate().skip(offset as usize) {
                    if reply.add(e.ino, (index + 1) as i64, kind(e.kind), &e.name) {
                        break;
                    }
                }
                reply.ok();
            }
            Err(e) => reply.error(e),
        }
    }
    fn releasedir(&mut self, req: &Request, ino: u64, fh: u64, _flags: u32, reply: ReplyEmpty) {
        empty(
            self.run(req, "releasedir", ino, json!({"fh":fh}), |s| {
                s.close(ino, fh)
            }),
            reply,
        );
    }
    fn fsyncdir(&mut self, req: &Request, ino: u64, fh: u64, _datasync: bool, reply: ReplyEmpty) {
        empty(
            self.run(req, "fsyncdir", ino, json!({"fh":fh}), |s| s.fsync(ino, fh)),
            reply,
        );
    }
    fn unlink(&mut self, req: &Request, parent: u64, name: &OsStr, reply: ReplyEmpty) {
        empty(
            self.run(
                req,
                "unlink",
                parent,
                json!({"name_bytes":name.as_bytes()}),
                |s| s.remove(parent, name, false),
            ),
            reply,
        );
    }
    fn rmdir(&mut self, req: &Request, parent: u64, name: &OsStr, reply: ReplyEmpty) {
        empty(
            self.run(
                req,
                "rmdir",
                parent,
                json!({"name_bytes":name.as_bytes()}),
                |s| s.remove(parent, name, true),
            ),
            reply,
        );
    }
    fn rename(
        &mut self,
        req: &Request,
        parent: u64,
        name: &OsStr,
        newparent: u64,
        newname: &OsStr,
        reply: ReplyEmpty,
    ) {
        empty(
            self.run(req,
                "rename",
                parent,
                json!({"name_bytes":name.as_bytes(),"new_parent":newparent,"new_name_bytes":newname.as_bytes()}),
                |s| s.rename(parent, name, newparent, newname),
            ),
            reply,
        );
    }
    fn statfs(&mut self, _req: &Request, _ino: u64, reply: ReplyStatfs) {
        let s = match self.service.lock() {
            Ok(s) => s,
            Err(_) => {
                reply.error(libc::EIO);
                return;
            }
        };
        let total = (s.capacity() / 512) as u64;
        let used = ((s.used_bytes() + 511) / 512) as u64;
        reply.statfs(
            total,
            total.saturating_sub(used),
            total.saturating_sub(used),
            s.node_count(),
            0,
            512,
            255,
            512,
        );
    }
}

trait Observed {
    fn bytes(&self) -> Option<u64> {
        None
    }
    fn inode(&self) -> Option<u64> {
        None
    }
}
impl Observed for () {}
impl Observed for u64 {}
impl Observed for u32 {
    fn bytes(&self) -> Option<u64> {
        Some(*self as u64)
    }
}
impl Observed for Vec<u8> {
    fn bytes(&self) -> Option<u64> {
        Some(self.len() as u64)
    }
}
impl Observed for Vec<crate::model::DirEntry> {}
impl Observed for Attr {
    fn inode(&self) -> Option<u64> {
        Some(self.ino)
    }
}
impl Observed for (Attr, u64) {
    fn inode(&self) -> Option<u64> {
        Some(self.0.ino)
    }
}
