//! 串行服务层：业务模型、同步边界、不可变快照和虚拟管理文件。
use crate::model::{
    Attr, DirEntry, FsResult, Kind, MemFs, CAPACITY, MAX_FILE_SIZE, ROOT, VIRTUAL_BASE,
};
use crate::store::{Snapshot, Store, MAX_SNAPSHOTS, SNAPSHOT_CAPACITY};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::{BTreeMap, HashMap};
use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::Path;
use std::time::Instant;
use time::Timespec;

pub const ADMIN: u64 = VIRTUAL_BASE;
const SNAPSHOTS: u64 = ADMIN + 1;
const STATUS: u64 = ADMIN + 2;
const CONTROL: u64 = ADMIN + 3;
const CONTROL_LIMIT: usize = 64 * 1024;

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum Command {
    Sync,
    SnapshotCreate {
        name: String,
    },
    SnapshotDelete {
        name: String,
    },
    Restore {
        snapshot: String,
        source: Vec<u8>,
        destination: Vec<u8>,
    },
}

struct View {
    saved: Snapshot,
    ids: HashMap<u64, u64>,
}
enum HandleData {
    Live(u64),
    Directory {
        entries: Vec<DirEntry>,
        live: Option<u64>,
    },
    Snapshot,
    Status(Vec<u8>),
    Control {
        bytes: Vec<u8>,
        outcome: Option<FsResult<()>>,
    },
}
struct Handle {
    ino: u64,
    view: Option<u64>,
    data: HandleData,
}

pub struct Service {
    live: MemFs,
    store: Option<Store>,
    active: BTreeMap<String, u64>,
    views: HashMap<u64, View>,
    virtual_nodes: HashMap<u64, (u64, usize)>,
    refs: HashMap<u64, u64>,
    handles: HashMap<u64, Handle>,
    next_ino: u64,
    next_fh: u64,
    started: Instant,
    created: Timespec,
    uid: u32,
    gid: u32,
    pub dirty: bool,
    pub last_sync_error: Option<String>,
    last_sync: Option<i64>,
    read_calls: u64,
    write_calls: u64,
    read_bytes: u64,
    write_bytes: u64,
}

impl Service {
    pub fn new(uid: u32, gid: u32, directory: Option<&Path>) -> Result<Self, String> {
        let (store, loaded) = match directory {
            Some(dir) => {
                let (store, loaded) = Store::open(dir)?;
                (Some(store), loaded)
            }
            None => (None, None),
        };
        let fresh = loaded.is_none();
        let live = match &loaded {
            Some(s) => MemFs::from_tree(&s.tree)?,
            None => MemFs::new(uid, gid),
        };
        let last_sync = loaded.as_ref().map(|s| s.synced);
        let mut result = Self {
            live,
            store,
            active: BTreeMap::new(),
            views: HashMap::new(),
            virtual_nodes: HashMap::new(),
            refs: HashMap::new(),
            handles: HashMap::new(),
            next_ino: ADMIN + 4,
            next_fh: 1,
            started: Instant::now(),
            created: time::get_time(),
            uid,
            gid,
            dirty: fresh,
            last_sync_error: None,
            last_sync,
            read_calls: 0,
            write_calls: 0,
            read_bytes: 0,
            write_bytes: 0,
        };
        if let Some(loaded) = loaded {
            for snapshot in loaded.snapshots {
                result.add_view(snapshot)?;
            }
        }
        if fresh {
            result
                .sync()
                .map_err(|_| result.last_sync_error.clone().unwrap_or_default())?;
        }
        Ok(result)
    }

    pub fn mode(&self) -> &'static str {
        if self.store.is_some() {
            "persistent"
        } else {
            "memory"
        }
    }

    fn add_view(&mut self, saved: Snapshot) -> Result<u64, String> {
        let root = self.next_ino;
        let mut ids = HashMap::new();
        // 树已按 inode 排序，根 inode=1 必须排在第一位。
        for (index, node) in saved.tree.nodes.iter().enumerate() {
            let ino = self.next_ino;
            self.next_ino = self
                .next_ino
                .checked_add(1)
                .ok_or("virtual inode exhausted")?;
            ids.insert(node.attr.ino, ino);
            self.virtual_nodes.insert(ino, (root, index));
        }
        self.active.insert(saved.name.clone(), root);
        self.views.insert(root, View { saved, ids });
        Ok(root)
    }

    fn snapshots(&self) -> Vec<Snapshot> {
        self.active
            .values()
            .map(|id| {
                let s = &self.views[id].saved;
                Snapshot {
                    name: s.name.clone(),
                    created: s.created,
                    tree: s.tree.clone(),
                }
            })
            .collect()
    }

    fn commit(&mut self, snapshots: &[Snapshot]) -> FsResult<()> {
        let now = time::get_time().sec;
        let tree = self.live.export();
        if let Some(store) = &mut self.store {
            if let Err(error) = store.save(&tree, snapshots, now) {
                self.last_sync_error = Some(error);
                self.dirty = true;
                return Err(libc::EIO);
            }
        }
        self.last_sync = Some(now);
        self.last_sync_error = None;
        self.dirty = false;
        Ok(())
    }

    pub fn sync(&mut self) -> FsResult<()> {
        self.commit(&self.snapshots())
    }

    fn snapshot_name(name: &str) -> FsResult<()> {
        MemFs::valid_name(OsStr::new(name))
    }

    pub fn command(&mut self, command: Command) -> FsResult<()> {
        match command {
            Command::Sync => self.sync(),
            Command::SnapshotCreate { name } => {
                Self::snapshot_name(&name)?;
                if self.active.contains_key(&name) {
                    return Err(libc::EEXIST);
                }
                let tree = self.live.export();
                let bytes: usize = tree.nodes.iter().map(|n| n.data.len()).sum();
                if self.active.len() >= MAX_SNAPSHOTS
                    || self.snapshot_bytes() + bytes > SNAPSHOT_CAPACITY
                {
                    return Err(libc::ENOSPC);
                }
                let mut snapshots = self.snapshots();
                snapshots.push(Snapshot {
                    name,
                    created: time::get_time().sec,
                    tree,
                });
                self.commit(&snapshots)?;
                self.add_view(snapshots.pop().unwrap())
                    .map_err(|_| libc::EOVERFLOW)?;
                Ok(())
            }
            Command::SnapshotDelete { name } => {
                let id = *self.active.get(&name).ok_or(libc::ENOENT)?;
                let snapshots: Vec<_> = self
                    .snapshots()
                    .into_iter()
                    .filter(|s| s.name != name)
                    .collect();
                self.commit(&snapshots)?;
                self.active.remove(&name);
                self.collect_view(id);
                Ok(())
            }
            Command::Restore {
                snapshot,
                source,
                destination,
            } => {
                let source = components(&source)?;
                let destination = components(&destination)?;
                let view = &self.views[self.active.get(&snapshot).ok_or(libc::ENOENT)?];
                let mut ino = ROOT;
                for part in source {
                    let parent = view
                        .saved
                        .tree
                        .nodes
                        .iter()
                        .find(|n| n.attr.ino == ino)
                        .ok_or(libc::ENOENT)?;
                    if parent.attr.kind != Kind::Directory {
                        return Err(libc::ENOTDIR);
                    }
                    ino = view
                        .saved
                        .tree
                        .nodes
                        .iter()
                        .find(|n| n.parent == ino && n.name == part)
                        .ok_or(libc::ENOENT)?
                        .attr
                        .ino;
                }
                let saved = view
                    .saved
                    .tree
                    .nodes
                    .iter()
                    .find(|n| n.attr.ino == ino)
                    .ok_or(libc::ENOENT)?
                    .clone();
                let mut parent = ROOT;
                for part in &destination[..destination.len() - 1] {
                    parent = self.live.lookup(parent, OsStr::from_bytes(part))?;
                }
                self.live.restore_file(
                    parent,
                    OsStr::from_bytes(destination.last().unwrap()),
                    &saved,
                )?;
                self.dirty = true;
                Ok(())
            }
        }
    }

    fn snapshot_bytes(&self) -> usize {
        self.active
            .values()
            .map(|id| {
                self.views[id]
                    .saved
                    .tree
                    .nodes
                    .iter()
                    .map(|n| n.data.len())
                    .sum::<usize>()
            })
            .sum()
    }

    fn status_bytes(&self) -> Vec<u8> {
        let (files, directories) = self.live.counts();
        let value = json!({
            "schema_version":1,"filesystem":"TeamFS","version":env!("CARGO_PKG_VERSION"),"mode":self.mode(),
            "uptime_seconds":self.started.elapsed().as_secs(),"files":files,"directories":directories,
            "used_bytes":self.live.used_bytes(),"capacity_bytes":CAPACITY,"max_file_bytes":MAX_FILE_SIZE,
            "snapshot_count":self.active.len(),"snapshot_bytes":self.snapshot_bytes(),"snapshot_limit":MAX_SNAPSHOTS,"snapshot_capacity_bytes":SNAPSHOT_CAPACITY,
            "snapshots":self.active.iter().map(|(name,id)|json!({"name":name,"created_unix":self.views[id].saved.created})).collect::<Vec<_>>(),
            "read_calls":self.read_calls,"write_calls":self.write_calls,"read_bytes":self.read_bytes,"write_bytes":self.write_bytes,
            "dirty":self.dirty,"last_sync_unix":self.last_sync,"last_sync_error":self.last_sync_error
        });
        let mut bytes = serde_json::to_vec_pretty(&value).unwrap();
        bytes.push(b'\n');
        bytes
    }

    fn virtual_attr(&self, ino: u64, kind: Kind, mode: u16, size: u64, nlink: u32) -> Attr {
        Attr {
            ino,
            size,
            kind,
            mode,
            uid: self.uid,
            gid: self.gid,
            nlink,
            atime: self.created,
            mtime: self.created,
            ctime: self.created,
            crtime: self.created,
        }
    }

    pub fn attr(&self, ino: u64) -> FsResult<Attr> {
        if ino < ADMIN {
            let mut attr = self.live.attr(ino)?;
            if ino == ROOT {
                attr.nlink += 1;
            }
            return Ok(attr);
        }
        match ino {
            ADMIN => Ok(self.virtual_attr(ino, Kind::Directory, 0o555, 0, 3)),
            SNAPSHOTS => {
                Ok(self.virtual_attr(ino, Kind::Directory, 0o555, 0, 2 + self.active.len() as u32))
            }
            STATUS => {
                Ok(self.virtual_attr(ino, Kind::File, 0o444, self.status_bytes().len() as u64, 1))
            }
            CONTROL => Ok(self.virtual_attr(ino, Kind::File, 0o200, 0, 1)),
            _ => {
                let (view, index) = self.virtual_nodes.get(&ino).ok_or(libc::ENOENT)?;
                let mut attr = self.views[view].saved.tree.nodes[*index].attr.clone();
                attr.ino = ino;
                attr.mode = if attr.kind == Kind::Directory {
                    0o555
                } else {
                    0o444
                };
                if ino == *view && !self.active.values().any(|v| v == view) {
                    attr.nlink = 0;
                }
                Ok(attr)
            }
        }
    }

    pub fn entries(&self, ino: u64) -> FsResult<Vec<DirEntry>> {
        let mut entries = if ino < ADMIN {
            let mut entries = self.live.entries(ino)?;
            if ino == ROOT {
                entries.push(entry(ADMIN, ".teamfs", Kind::Directory));
            }
            return Ok(entries);
        } else {
            Vec::new()
        };
        let parent = match ino {
            ADMIN => ROOT,
            SNAPSHOTS => ADMIN,
            STATUS | CONTROL => return Err(libc::ENOTDIR),
            _ => {
                let (view, index) = self.virtual_nodes.get(&ino).ok_or(libc::ENOENT)?;
                let view_data = &self.views[view];
                let node = &view_data.saved.tree.nodes[*index];
                if node.attr.kind != Kind::Directory {
                    return Err(libc::ENOTDIR);
                }
                if ino == *view {
                    SNAPSHOTS
                } else {
                    view_data.ids[&node.parent]
                }
            }
        };
        entries.push(entry(ino, ".", Kind::Directory));
        entries.push(entry(parent, "..", Kind::Directory));
        match ino {
            ADMIN => {
                entries.push(entry(SNAPSHOTS, "snapshots", Kind::Directory));
                entries.push(entry(STATUS, "status.json", Kind::File));
                entries.push(entry(CONTROL, "control", Kind::File));
            }
            SNAPSHOTS => {
                for (name, id) in &self.active {
                    entries.push(entry(*id, name, Kind::Directory));
                }
            }
            _ => {
                let (view, index) = self.virtual_nodes[&ino];
                let v = &self.views[&view];
                let old = v.saved.tree.nodes[index].attr.ino;
                for n in &v.saved.tree.nodes {
                    if n.attr.ino != ROOT && n.parent == old {
                        entries.push(DirEntry {
                            ino: v.ids[&n.attr.ino],
                            name: OsString::from_vec(n.name.clone()),
                            kind: n.attr.kind,
                        });
                    }
                }
            }
        }
        Ok(entries)
    }

    pub fn lookup(&self, parent: u64, name: &OsStr) -> FsResult<u64> {
        if parent < ADMIN {
            if parent == ROOT && name == ".teamfs" {
                return Ok(ADMIN);
            }
            return self.live.lookup(parent, name);
        }
        self.entries(parent)?
            .into_iter()
            .find(|e| e.name == name)
            .map(|e| e.ino)
            .ok_or(libc::ENOENT)
    }

    pub fn remember(&mut self, ino: u64) {
        if ino < ADMIN {
            self.live.remember(ino);
        } else {
            *self.refs.entry(ino).or_default() += 1;
        }
    }
    pub fn forget(&mut self, ino: u64, count: u64) {
        if ino < ADMIN {
            self.live.forget(ino, count);
            return;
        }
        if let Some(value) = self.refs.get_mut(&ino) {
            *value = value.saturating_sub(count);
            if *value == 0 {
                self.refs.remove(&ino);
            }
        }
        if let Some((view, _)) = self.virtual_nodes.get(&ino).copied() {
            self.collect_view(view);
        }
    }
    fn collect_view(&mut self, view: u64) {
        if self.active.values().any(|v| *v == view)
            || self.handles.values().any(|h| h.view == Some(view))
            || self.refs.iter().any(|(ino, count)| {
                *count > 0 && self.virtual_nodes.get(ino).map(|v| v.0) == Some(view)
            })
        {
            return;
        }
        self.views.remove(&view);
        self.virtual_nodes.retain(|_, value| value.0 != view);
    }

    fn add_handle(&mut self, ino: u64, data: HandleData) -> u64 {
        let fh = self.next_fh;
        self.next_fh += 1;
        self.handles.insert(
            fh,
            Handle {
                ino,
                view: self.virtual_nodes.get(&ino).map(|v| v.0),
                data,
            },
        );
        fh
    }
    pub fn check_handle(&self, ino: u64, fh: u64) -> FsResult<()> {
        let h = self
            .handles
            .get(&fh)
            .filter(|h| h.ino == ino)
            .ok_or(libc::EBADF)?;
        match h.data {
            HandleData::Live(local)
            | HandleData::Directory {
                live: Some(local), ..
            } => self.live.check_handle(ino, local),
            _ => Ok(()),
        }
    }
    pub fn open(&mut self, ino: u64, flags: i32) -> FsResult<u64> {
        if ino < ADMIN {
            let fh = self.live.open(ino, flags)?;
            if flags & libc::O_TRUNC != 0 && flags & libc::O_ACCMODE != libc::O_RDONLY {
                self.dirty = true;
            }
            return Ok(self.add_handle(ino, HandleData::Live(fh)));
        }
        if self.attr(ino)?.kind == Kind::Directory {
            return Err(libc::EISDIR);
        }
        let data = if ino == CONTROL {
            if flags & libc::O_ACCMODE != libc::O_WRONLY {
                return Err(libc::EACCES);
            }
            HandleData::Control {
                bytes: Vec::new(),
                outcome: None,
            }
        } else {
            if flags & libc::O_ACCMODE != libc::O_RDONLY || flags & libc::O_TRUNC != 0 {
                return Err(libc::EROFS);
            }
            if ino == STATUS {
                HandleData::Status(self.status_bytes())
            } else {
                HandleData::Snapshot
            }
        };
        Ok(self.add_handle(ino, data))
    }
    pub fn open_dir(&mut self, ino: u64, flags: i32) -> FsResult<u64> {
        let live = if ino < ADMIN {
            Some(self.live.open_dir(ino, flags)?)
        } else {
            None
        };
        let entries = if let Some(local) = live {
            let mut entries = self.live.directory_entries(ino, local)?.to_vec();
            if ino == ROOT {
                entries.push(entry(ADMIN, ".teamfs", Kind::Directory));
            }
            entries
        } else {
            self.entries(ino)?
        };
        Ok(self.add_handle(ino, HandleData::Directory { entries, live }))
    }
    pub fn directory_entries(&self, ino: u64, fh: u64) -> FsResult<&[DirEntry]> {
        self.check_handle(ino, fh)?;
        match &self.handles[&fh].data {
            HandleData::Directory { entries, .. } => Ok(entries),
            _ => Err(libc::ENOTDIR),
        }
    }
    pub fn close(&mut self, ino: u64, fh: u64) -> FsResult<()> {
        self.check_handle(ino, fh)?;
        let h = self.handles.remove(&fh).unwrap();
        match h.data {
            HandleData::Live(local)
            | HandleData::Directory {
                live: Some(local), ..
            } => self.live.close(ino, local)?,
            _ => (),
        }
        if let Some(view) = h.view {
            self.collect_view(view);
        }
        Ok(())
    }
    pub fn read(&mut self, ino: u64, fh: u64, offset: i64, size: u32) -> FsResult<Vec<u8>> {
        self.check_handle(ino, fh)?;
        if offset < 0 {
            return Err(libc::EINVAL);
        }
        let data = match &self.handles[&fh].data {
            HandleData::Live(local) => {
                let bytes = self.live.read(ino, *local, offset, size)?;
                self.dirty = true;
                bytes
            }
            HandleData::Snapshot => {
                let (view, index) = self.virtual_nodes[&ino];
                slice(
                    &self.views[&view].saved.tree.nodes[index].data,
                    offset,
                    size,
                )
            }
            HandleData::Status(bytes) => return Ok(slice(bytes, offset, size)),
            _ => return Err(libc::EBADF),
        };
        self.read_calls += 1;
        self.read_bytes += data.len() as u64;
        Ok(data)
    }
    pub fn write(&mut self, ino: u64, fh: u64, offset: i64, bytes: &[u8]) -> FsResult<u32> {
        self.check_handle(ino, fh)?;
        match &mut self.handles.get_mut(&fh).unwrap().data {
            HandleData::Live(local) => {
                let result = self.live.write(ino, *local, offset, bytes)?;
                self.write_calls += 1;
                self.write_bytes += result as u64;
                if result > 0 {
                    self.dirty = true;
                }
                Ok(result)
            }
            HandleData::Control {
                bytes: buffer,
                outcome,
            } => {
                if outcome.is_some() {
                    return Err(libc::EBUSY);
                }
                if offset < 0 || offset as usize != buffer.len() {
                    return Err(libc::EINVAL);
                }
                if buffer.len().saturating_add(bytes.len()) > CONTROL_LIMIT {
                    return Err(libc::EFBIG);
                }
                buffer.extend_from_slice(bytes);
                Ok(bytes.len() as u32)
            }
            _ => Err(libc::EROFS),
        }
    }
    pub fn fsync(&mut self, ino: u64, fh: u64) -> FsResult<()> {
        self.check_handle(ino, fh)?;
        let parsed = match &self.handles[&fh].data {
            HandleData::Control { bytes, outcome } => {
                if let Some(result) = outcome {
                    return *result;
                }
                Some(serde_json::from_slice::<Command>(bytes).map_err(|_| libc::EINVAL))
            }
            _ => None,
        };
        if let Some(parsed) = parsed {
            let result = parsed.and_then(|command| self.command(command));
            if let HandleData::Control { outcome, .. } =
                &mut self.handles.get_mut(&fh).unwrap().data
            {
                *outcome = Some(result);
            }
            result
        } else {
            self.sync()
        }
    }
    fn writable_entry(&self, parent: u64, name: &OsStr) -> FsResult<()> {
        if parent >= ADMIN || (parent == ROOT && name == ".teamfs") {
            Err(libc::EROFS)
        } else {
            Ok(())
        }
    }
    pub fn create(
        &mut self,
        parent: u64,
        name: &OsStr,
        kind: Kind,
        mode: u32,
        uid: u32,
        gid: u32,
    ) -> FsResult<u64> {
        self.writable_entry(parent, name)?;
        let ino = self.live.create(parent, name, kind, mode, uid, gid)?;
        self.dirty = true;
        Ok(ino)
    }
    pub fn truncate(&mut self, ino: u64, size: u64) -> FsResult<()> {
        if ino >= ADMIN {
            return Err(libc::EROFS);
        }
        self.live.truncate(ino, size)?;
        self.dirty = true;
        Ok(())
    }
    pub fn set_metadata(
        &mut self,
        ino: u64,
        mode: Option<u32>,
        uid: Option<u32>,
        gid: Option<u32>,
        atime: Option<Timespec>,
        mtime: Option<Timespec>,
    ) -> FsResult<Attr> {
        if ino >= ADMIN {
            return Err(libc::EROFS);
        }
        self.live.set_metadata(ino, mode, uid, gid, atime, mtime)?;
        self.dirty = true;
        self.attr(ino)
    }
    pub fn remove(&mut self, parent: u64, name: &OsStr, directory: bool) -> FsResult<()> {
        self.writable_entry(parent, name)?;
        self.live.remove(parent, name, directory)?;
        self.dirty = true;
        Ok(())
    }
    pub fn rename(
        &mut self,
        parent: u64,
        name: &OsStr,
        new_parent: u64,
        new_name: &OsStr,
    ) -> FsResult<()> {
        self.writable_entry(parent, name)?;
        self.writable_entry(new_parent, new_name)?;
        self.live.rename(parent, name, new_parent, new_name)?;
        self.dirty = true;
        Ok(())
    }
    pub fn used_bytes(&self) -> usize {
        self.live.used_bytes()
    }
    pub fn node_count(&self) -> u64 {
        self.live.node_count()
    }
}

fn entry(ino: u64, name: &str, kind: Kind) -> DirEntry {
    DirEntry {
        ino,
        name: OsString::from(name),
        kind,
    }
}
fn slice(bytes: &[u8], offset: i64, size: u32) -> Vec<u8> {
    let start = (offset as u64).min(bytes.len() as u64) as usize;
    bytes[start..start.saturating_add(size as usize).min(bytes.len())].to_vec()
}
fn components(path: &[u8]) -> FsResult<Vec<&[u8]>> {
    let parts: Vec<_> = path.split(|b| *b == b'/').collect();
    if parts.is_empty() || parts[0] == b".teamfs" {
        return Err(libc::EINVAL);
    }
    for part in &parts {
        MemFs::valid_name(OsStr::from_bytes(part))?;
    }
    Ok(parts)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn retired_snapshot_survives_handle_and_lookup_then_collects() {
        let mut service = Service::new(1000, 1000, None).unwrap();
        service
            .command(Command::SnapshotCreate { name: "old".into() })
            .unwrap();
        let root = service.lookup(SNAPSHOTS, OsStr::new("old")).unwrap();
        let ino = service.lookup(root, OsStr::new("welcome.txt")).unwrap();
        service.remember(ino);
        let fh = service.open(ino, libc::O_RDONLY).unwrap();
        service
            .command(Command::SnapshotDelete { name: "old".into() })
            .unwrap();
        assert!(!service.read(ino, fh, 0, 4096).unwrap().is_empty());
        service.close(ino, fh).unwrap();
        assert!(service.views.contains_key(&root));
        service.forget(ino, 1);
        assert!(!service.views.contains_key(&root));
        assert!(!service.virtual_nodes.contains_key(&ino));
    }
    #[test]
    fn invalid_and_oversized_control_requests_do_not_execute() {
        let mut service = Service::new(1000, 1000, None).unwrap();
        let fh = service.open(CONTROL, libc::O_WRONLY).unwrap();
        assert_eq!(
            service.write(CONTROL, fh, 0, &vec![b'x'; CONTROL_LIMIT + 1]),
            Err(libc::EFBIG)
        );
        service.write(CONTROL, fh, 0, b"{}").unwrap();
        assert_eq!(service.fsync(CONTROL, fh), Err(libc::EINVAL));
        assert_eq!(service.fsync(CONTROL, fh), Err(libc::EINVAL));
        assert_eq!(service.write(CONTROL, fh, 2, b"x"), Err(libc::EBUSY));
        assert!(service.active.is_empty());
        for p in [
            b"../escape".as_slice(),
            b"/absolute",
            b".teamfs/status.json",
            b"a//b",
            b"",
            b"a/./b",
        ] {
            assert!(components(p).is_err());
        }
    }
}
