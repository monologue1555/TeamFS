//! FUSE 适配层：回调统一进入 Service，持久化和管理请求共享同一份状态。
use crate::model::{Attr, FsResult, Kind, CAPACITY};
use crate::service::Service;
use fuse::{
    FileAttr, FileType, Filesystem, ReplyAttr, ReplyCreate, ReplyData, ReplyDirectory, ReplyEmpty,
    ReplyEntry, ReplyOpen, ReplyStatfs, ReplyWrite, Request,
};
use std::{cell::RefCell, ffi::OsStr, rc::Rc};
use time::Timespec;
const TTL: Timespec = Timespec { sec: 0, nsec: 0 };
const DIRECT_IO: u32 = 1;
pub struct TeamFs {
    service: Rc<RefCell<Service>>,
    trace: bool,
    sequence: u64,
}
impl TeamFs {
    pub fn new(service: Rc<RefCell<Service>>, trace: bool) -> Self {
        Self {
            service,
            trace,
            sequence: 0,
        }
    }
    fn run<T>(
        &mut self,
        op: &str,
        ino: u64,
        detail: String,
        f: impl FnOnce(&mut Service) -> FsResult<T>,
    ) -> FsResult<T> {
        let result = f(&mut self.service.borrow_mut());
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
    fn lookup(&mut self, _req: &Request, parent: u64, name: &OsStr, reply: ReplyEntry) {
        let result = self.run("lookup", parent, format!("name={name:?}"), |s| {
            let ino = s.lookup(parent, name)?;
            let a = s.attr(ino)?;
            s.remember(ino);
            Ok(a)
        });
        match result {
            Ok(a) => reply.entry(&TTL, &attr(a), 0),
            Err(e) => reply.error(e),
        }
    }
    fn forget(&mut self, _req: &Request, ino: u64, nlookup: u64) {
        let _ = self.run("forget", ino, format!("nlookup={nlookup}"), |s| {
            s.forget(ino, nlookup);
            Ok(())
        });
    }
    fn getattr(&mut self, _req: &Request, ino: u64, reply: ReplyAttr) {
        match self.run("getattr", ino, String::new(), |s| s.attr(ino)) {
            Ok(a) => reply.attr(&TTL, &attr(a)),
            Err(e) => reply.error(e),
        }
    }
    fn setattr(
        &mut self,
        _req: &Request,
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
            "setattr",
            ino,
            format!("size={size:?} mode={mode:?}"),
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
        let result = self.run("mkdir", parent, format!("name={name:?}"), |s| {
            let ino = s.create(parent, name, Kind::Directory, mode, req.uid(), req.gid())?;
            let a = s.attr(ino)?;
            s.remember(ino);
            Ok(a)
        });
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
        let result = self.run("mknod", parent, format!("name={name:?}"), |s| {
            if mode & libc::S_IFMT != libc::S_IFREG {
                return Err(libc::EOPNOTSUPP);
            }
            let ino = s.create(parent, name, Kind::File, mode, req.uid(), req.gid())?;
            let a = s.attr(ino)?;
            s.remember(ino);
            Ok(a)
        });
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
            "create",
            parent,
            format!("name={name:?} flags={flags:#x}"),
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
            Ok((a, fh)) => reply.created(&TTL, &attr(a), 0, fh, DIRECT_IO),
            Err(e) => reply.error(e),
        }
    }
    fn open(&mut self, _req: &Request, ino: u64, flags: u32, reply: ReplyOpen) {
        match self.run("open", ino, format!("flags={flags:#x}"), |s| {
            s.open(ino, flags as i32)
        }) {
            Ok(fh) => reply.opened(fh, DIRECT_IO),
            Err(e) => reply.error(e),
        }
    }
    fn read(
        &mut self,
        _req: &Request,
        ino: u64,
        fh: u64,
        offset: i64,
        size: u32,
        reply: ReplyData,
    ) {
        match self.run(
            "read",
            ino,
            format!("fh={fh} offset={offset} requested={size}"),
            |s| s.read(ino, fh, offset, size),
        ) {
            Ok(data) => reply.data(&data),
            Err(e) => reply.error(e),
        }
    }
    fn write(
        &mut self,
        _req: &Request,
        ino: u64,
        fh: u64,
        offset: i64,
        data: &[u8],
        _flags: u32,
        reply: ReplyWrite,
    ) {
        match self.run(
            "write",
            ino,
            format!("fh={fh} offset={offset} bytes={}", data.len()),
            |s| s.write(ino, fh, offset, data),
        ) {
            Ok(size) => reply.written(size),
            Err(e) => reply.error(e),
        }
    }
    fn flush(&mut self, _req: &Request, ino: u64, fh: u64, _lock_owner: u64, reply: ReplyEmpty) {
        empty(
            self.run("flush", ino, format!("fh={fh} (not a sync point)"), |s| {
                s.check_handle(ino, fh)
            }),
            reply,
        );
    }
    fn release(
        &mut self,
        _req: &Request,
        ino: u64,
        fh: u64,
        _flags: u32,
        _lock_owner: u64,
        _flush: bool,
        reply: ReplyEmpty,
    ) {
        empty(
            self.run("release", ino, format!("fh={fh}"), |s| s.close(ino, fh)),
            reply,
        );
    }
    fn fsync(&mut self, _req: &Request, ino: u64, fh: u64, _datasync: bool, reply: ReplyEmpty) {
        empty(
            self.run("fsync", ino, format!("fh={fh}"), |s| s.fsync(ino, fh)),
            reply,
        );
    }
    fn opendir(&mut self, _req: &Request, ino: u64, flags: u32, reply: ReplyOpen) {
        match self.run("opendir", ino, String::new(), |s| {
            s.open_dir(ino, flags as i32)
        }) {
            Ok(fh) => reply.opened(fh, 0),
            Err(e) => reply.error(e),
        }
    }
    fn readdir(
        &mut self,
        _req: &Request,
        ino: u64,
        fh: u64,
        offset: i64,
        mut reply: ReplyDirectory,
    ) {
        let result = self.run("readdir", ino, format!("fh={fh} offset={offset}"), |s| {
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
    fn releasedir(&mut self, _req: &Request, ino: u64, fh: u64, _flags: u32, reply: ReplyEmpty) {
        empty(
            self.run("releasedir", ino, format!("fh={fh}"), |s| s.close(ino, fh)),
            reply,
        );
    }
    fn fsyncdir(&mut self, _req: &Request, ino: u64, fh: u64, _datasync: bool, reply: ReplyEmpty) {
        empty(
            self.run("fsyncdir", ino, format!("fh={fh}"), |s| s.fsync(ino, fh)),
            reply,
        );
    }
    fn unlink(&mut self, _req: &Request, parent: u64, name: &OsStr, reply: ReplyEmpty) {
        empty(
            self.run("unlink", parent, format!("name={name:?}"), |s| {
                s.remove(parent, name, false)
            }),
            reply,
        );
    }
    fn rmdir(&mut self, _req: &Request, parent: u64, name: &OsStr, reply: ReplyEmpty) {
        empty(
            self.run("rmdir", parent, format!("name={name:?}"), |s| {
                s.remove(parent, name, true)
            }),
            reply,
        );
    }
    fn rename(
        &mut self,
        _req: &Request,
        parent: u64,
        name: &OsStr,
        newparent: u64,
        newname: &OsStr,
        reply: ReplyEmpty,
    ) {
        empty(
            self.run(
                "rename",
                parent,
                format!("name={name:?} new_parent={newparent} new_name={newname:?}"),
                |s| s.rename(parent, name, newparent, newname),
            ),
            reply,
        );
    }
    fn statfs(&mut self, _req: &Request, _ino: u64, reply: ReplyStatfs) {
        let s = self.service.borrow();
        let total = (CAPACITY / 512) as u64;
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
