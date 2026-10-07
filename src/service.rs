//! 串行服务层：业务模型、同步边界、不可变快照和虚拟管理文件。
use crate::history::{self, TrashRecord, TRASH_CAPACITY, TRASH_LIMIT};
use crate::model::{Attr, DirEntry, FsResult, Kind, MemFs, ROOT, VIRTUAL_BASE};
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
const TRASH: u64 = ADMIN + 4;
const TRASH_INDEX: u64 = ADMIN + 5;
const DIFFS: u64 = ADMIN + 6;
const EVENTS: u64 = ADMIN + 7;
const METRICS: u64 = ADMIN + 8;
const CONTROL_LIMIT: usize = 64 * 1024;

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum Command {
    RestoreTree {
        snapshot: String,
        source: Vec<u8>,
        destination: Vec<u8>,
        dry_run: bool,
    },
    Sync,
    TrashRestore {
        id: u64,
        destination: Vec<u8>,
    },
    TrashPurge {
        id: u64,
    },
    TrashPurgeAll,
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
    diff_ino: u64,
}
enum HandleData {
    Live(u64),
    Directory {
        entries: Vec<DirEntry>,
        live: Option<u64>,
    },
    Snapshot,
    Frozen(Vec<u8>),
    Trash,
    Control {
        bytes: Vec<u8>,
        outcome: Option<FsResult<()>>,
        response: Vec<u8>,
    },
}
struct Handle {
    ino: u64,
    view: Option<u64>,
    data: HandleData,
}

pub struct Service {
    pub audit: crate::audit::Audit,
    pub metrics: crate::metrics::Metrics,
    pub caller: Option<crate::audit::Caller>,
    pub operation_context: Option<String>,
    live: MemFs,
    store: Option<Store>,
    active: BTreeMap<String, u64>,
    views: HashMap<u64, View>,
    virtual_nodes: HashMap<u64, (u64, usize)>,
    diff_nodes: HashMap<u64, u64>,
    trash: BTreeMap<u64, u64>,
    trash_nodes: HashMap<u64, TrashRecord>,
    next_trash_id: u64,
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
    auto_sync_interval: Option<u64>,
    auto_sync_attempts: u64,
    auto_sync_successes: u64,
}

impl Service {
    pub fn capacity(&self) -> usize {
        self.live.capacity
    }
    pub fn configure_limits(
        &mut self,
        capacity: Option<usize>,
        max_file: Option<usize>,
    ) -> Result<(), String> {
        if capacity.is_some() || max_file.is_some() {
            self.live.configure_limits(
                capacity.unwrap_or(self.live.capacity),
                max_file.unwrap_or(self.live.max_file),
            )?;
            self.dirty = true;
        }
        Ok(())
    }
    pub fn symlink(
        &mut self,
        parent: u64,
        name: &OsStr,
        target: &[u8],
        uid: u32,
        gid: u32,
    ) -> FsResult<u64> {
        self.writable_entry(parent, name)?;
        let ino = self.live.symlink(parent, name, target, uid, gid)?;
        self.dirty = true;
        Ok(ino)
    }
    pub fn readlink(&self, ino: u64) -> FsResult<Vec<u8>> {
        if ino < ADMIN {
            return self.live.readlink(ino);
        }
        if let Some(record) = self.trash_nodes.get(&ino) {
            if record.attr.kind != Kind::Symlink {
                return Err(libc::EINVAL);
            }
            return record.data.all();
        }
        let (view, index) = self.virtual_nodes.get(&ino).ok_or(libc::EINVAL)?;
        let node = &self.views[view].saved.tree.nodes[*index];
        if node.attr.kind != Kind::Symlink {
            return Err(libc::EINVAL);
        }
        node.data.all()
    }
    fn restore_tree_output(
        &mut self,
        snapshot: &str,
        source: &[u8],
        destination: &[u8],
        dry_run: bool,
    ) -> FsResult<Vec<u8>> {
        let view = &self.views[self.active.get(snapshot).ok_or(libc::ENOENT)?];
        let mut source_ino = ROOT;
        if source != b"." {
            for part in components(source)? {
                let parent = view
                    .saved
                    .tree
                    .nodes
                    .iter()
                    .find(|n| n.attr.ino == source_ino)
                    .ok_or(libc::ENOENT)?;
                if parent.attr.kind != Kind::Directory {
                    return Err(libc::ENOTDIR);
                }
                source_ino = view
                    .saved
                    .tree
                    .nodes
                    .iter()
                    .find(|n| n.attr.ino != ROOT && n.parent == source_ino && n.name == part)
                    .ok_or(libc::ENOENT)?
                    .attr
                    .ino;
            }
        }
        let root = view
            .saved
            .tree
            .nodes
            .iter()
            .find(|n| n.attr.ino == source_ino)
            .ok_or(libc::ENOENT)?
            .clone();
        if root.attr.kind != Kind::Directory {
            return Err(libc::ENOTDIR);
        }
        let mut nodes = vec![root];
        let mut paths = vec![destination.to_vec()];
        let mut cursor = 0;
        while cursor < nodes.len() {
            let parent_ino = nodes[cursor].attr.ino;
            for node in view
                .saved
                .tree
                .nodes
                .iter()
                .filter(|n| n.attr.ino != ROOT && n.parent == parent_ino)
            {
                let mut path = paths[cursor].clone();
                path.push(b'/');
                path.extend_from_slice(&node.name);
                paths.push(path);
                nodes.push(node.clone());
            }
            cursor += 1;
        }
        let parts = components(destination)?;
        let mut parent = ROOT;
        let mut error = None;
        for part in &parts[..parts.len() - 1] {
            match self.live.lookup(parent, OsStr::from_bytes(part)) {
                Ok(ino) => parent = ino,
                Err(e) => {
                    error = Some(e);
                    break;
                }
            }
        }
        if error.is_none() {
            error = self
                .live
                .validate_tree_restore(parent, OsStr::from_bytes(parts.last().unwrap()), &nodes)
                .err();
        }
        let preview = json!({"snapshot":snapshot,"source_bytes":source,"destination_bytes":destination,
            "dry_run":dry_run,"can_restore":error.is_none(),"error_errno":error,
            "files":nodes.iter().filter(|n|n.attr.kind==Kind::File).count(),
            "directories":nodes.iter().filter(|n|n.attr.kind==Kind::Directory).count(),
            "symlinks":nodes.iter().filter(|n|n.attr.kind==Kind::Symlink).count(),
            "bytes":nodes.iter().map(|n|n.data.len()).sum::<usize>(),
            "entries":paths.iter().zip(&nodes).map(|(p,n)|json!({"path_bytes":p,"path_display":history::display_path(p),"kind":n.attr.kind,"size":n.attr.size})).collect::<Vec<_>>()});
        if !dry_run {
            if let Some(error) = error {
                return Err(error);
            }
            self.live
                .restore_tree(parent, OsStr::from_bytes(parts.last().unwrap()), &nodes)?;
            self.dirty = true;
        }
        Ok(history::json_bytes(&preview))
    }
    fn command_output(&mut self, command: Command) -> FsResult<Vec<u8>> {
        if let Command::RestoreTree {
            snapshot,
            source,
            destination,
            dry_run,
        } = command
        {
            return self.restore_tree_output(&snapshot, &source, &destination, dry_run);
        }
        self.command(command)?;
        Ok(Vec::new())
    }
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
            Some(s) => MemFs::from_tree_limits(&s.tree, s.capacity, s.max_file)?,
            None => MemFs::new(uid, gid),
        };
        let last_sync = loaded.as_ref().map(|s| s.synced);
        let mut result = Self {
            audit: crate::audit::Audit::new(),
            metrics: crate::metrics::Metrics::default(),
            caller: None,
            operation_context: Some("initialize".into()),
            live,
            store,
            active: BTreeMap::new(),
            views: HashMap::new(),
            virtual_nodes: HashMap::new(),
            diff_nodes: HashMap::new(),
            trash: BTreeMap::new(),
            trash_nodes: HashMap::new(),
            next_trash_id: loaded.as_ref().map_or(1, |s| s.next_trash_id),
            refs: HashMap::new(),
            handles: HashMap::new(),
            next_ino: ADMIN + 9,
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
            auto_sync_interval: None,
            auto_sync_attempts: 0,
            auto_sync_successes: 0,
        };
        if let Some(loaded) = loaded {
            for snapshot in loaded.snapshots {
                result.add_view(snapshot)?;
            }
            for record in loaded.trash {
                let ino = result
                    .allocate_virtual()
                    .map_err(|_| "virtual inode exhausted")?;
                result.trash.insert(record.id, ino);
                result.trash_nodes.insert(ino, record);
            }
        }
        if fresh {
            result
                .sync()
                .map_err(|_| result.last_sync_error.clone().unwrap_or_default())?;
        }
        result.operation_context = None;
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
        self.next_ino
            .checked_add(saved.tree.nodes.len() as u64 + 1)
            .ok_or("virtual inode exhausted")?;
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
        let diff_ino = self
            .allocate_virtual()
            .map_err(|_| "virtual inode exhausted")?;
        self.diff_nodes.insert(diff_ino, root);
        self.views.insert(
            root,
            View {
                saved,
                ids,
                diff_ino,
            },
        );
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

    fn records(&self) -> Vec<TrashRecord> {
        self.trash
            .values()
            .map(|ino| self.trash_nodes[ino].clone())
            .collect()
    }

    fn commit(&mut self, snapshots: &[Snapshot]) -> FsResult<()> {
        self.commit_with_trash(snapshots, &self.records())
    }

    fn commit_with_trash(&mut self, snapshots: &[Snapshot], trash: &[TrashRecord]) -> FsResult<()> {
        let began = Instant::now();
        let now = time::get_time().sec;
        let tree = self.live.export();
        if let Some(store) = &mut self.store {
            if let Err(error) = store.save(
                &tree,
                snapshots,
                trash,
                self.next_trash_id,
                now,
                self.live.capacity,
                self.live.max_file,
            ) {
                self.last_sync_error = Some(error);
                self.dirty = true;
                self.record_commit(began, Some(libc::EIO));
                return Err(libc::EIO);
            }
        }
        self.last_sync = Some(now);
        self.last_sync_error = None;
        self.dirty = false;
        self.record_commit(began, None);
        Ok(())
    }
    fn record_commit(&mut self, began: Instant, error: Option<i32>) {
        let duration = began.elapsed().as_micros() as u64;
        self.metrics.record(
            "sync.commit",
            None,
            self.caller.as_ref().map(|c| c.pid),
            error,
            crate::metrics::Timing {
                total_us: duration,
                service_us: duration,
                commit_us: duration,
                lock_wait_us: 0,
            },
        );
        self.audit.record("sync","commit",self.caller.clone(),None,None,None,
            json!({"trigger":self.operation_context,"mode":self.mode(),"storage_error":self.last_sync_error,
                "committed_content_bytes":if error.is_some(){0}else{self.store.as_ref().map_or(0,|s|s.last_content_bytes)}}),
            error,None,duration,self.dirty);
    }
    pub fn sync_named(&mut self, name: &str) -> FsResult<()> {
        let previous = self.operation_context.replace(name.into());
        let result = self.sync();
        self.operation_context = previous;
        result
    }
    pub fn lifecycle(&mut self, name: &str, error: Option<i32>) {
        self.audit.record(
            "lifecycle",
            name,
            None,
            None,
            None,
            None,
            json!({"mode":self.mode()}),
            error,
            None,
            0,
            self.dirty,
        );
    }
    pub fn audit_path(&self, ino: u64, name: Option<&[u8]>) -> Option<Vec<u8>> {
        let mut path = if ino < ADMIN {
            self.live.path_of(ino)
        } else if let Some((view, index)) = self.virtual_nodes.get(&ino) {
            let v = &self.views[view];
            if self.active.get(&v.saved.name) != Some(view) {
                return None;
            }
            let mut n = &v.saved.tree.nodes[*index];
            let mut parts = Vec::new();
            while n.attr.ino != ROOT {
                parts.push(n.name.clone());
                n = v.saved.tree.nodes.iter().find(|p| p.attr.ino == n.parent)?;
            }
            parts.reverse();
            let mut path = format!(".teamfs/snapshots/{}", v.saved.name).into_bytes();
            if !parts.is_empty() {
                path.push(b'/');
                path.extend(parts.join(&b'/'));
            }
            Some(path)
        } else if let Some(r) = self.trash_nodes.get(&ino) {
            if self.trash.get(&r.id) != Some(&ino) {
                return None;
            }
            Some(format!(".teamfs/trash/{}", r.id).into_bytes())
        } else {
            None
        }?;
        if let Some(name) = name {
            if !path.is_empty() {
                path.push(b'/');
            }
            path.extend_from_slice(name);
        }
        Some(path)
    }
    pub fn audit_visible(&self, ino: u64, path: Option<&[u8]>) -> bool {
        (ino < ADMIN && !path.map_or(false, |p| p == b".teamfs" || p.starts_with(b".teamfs/")))
            || self.virtual_nodes.contains_key(&ino)
            || self.trash_nodes.contains_key(&ino)
    }

    pub fn sync(&mut self) -> FsResult<()> {
        self.commit(&self.snapshots())
    }

    pub fn configure_auto_sync(&mut self, seconds: Option<u64>) {
        self.auto_sync_interval = seconds;
    }
    pub fn auto_sync(&mut self) {
        if !self.dirty {
            return;
        }
        self.auto_sync_attempts += 1;
        if self.sync_named("auto_sync").is_ok() {
            self.auto_sync_successes += 1;
        } else {
            eprintln!(
                "TeamFS 自动同步失败：{}",
                self.last_sync_error.as_deref().unwrap_or("unknown")
            );
        }
    }
    fn allocate_virtual(&mut self) -> FsResult<u64> {
        let ino = self.next_ino;
        self.next_ino = ino.checked_add(1).ok_or(libc::EOVERFLOW)?;
        Ok(ino)
    }
    fn trash_bytes(&self) -> usize {
        self.trash
            .values()
            .map(|ino| self.trash_nodes[ino].data.len())
            .sum()
    }
    fn prepare_trash(&mut self, ino: u64, reason: &str) -> FsResult<(u64, TrashRecord)> {
        if self.trash.len() >= TRASH_LIMIT {
            return Err(libc::ENOSPC);
        }
        let size = self.live.attr(ino)?.size as usize;
        if self.trash_bytes().checked_add(size).ok_or(libc::ENOSPC)? > TRASH_CAPACITY {
            return Err(libc::ENOSPC);
        }
        if self.next_trash_id >= i64::MAX as u64 {
            return Err(libc::EOVERFLOW);
        }
        let (path, saved) = self.live.saved_file(ino)?;
        let virtual_ino = self.allocate_virtual()?;
        Ok((
            virtual_ino,
            TrashRecord {
                id: self.next_trash_id,
                path,
                deleted: time::get_time().sec,
                reason: reason.into(),
                attr: saved.attr,
                data: saved.data,
            },
        ))
    }
    fn accept_trash(&mut self, ino: u64, record: TrashRecord) {
        self.next_trash_id += 1;
        self.trash.insert(record.id, ino);
        self.trash_nodes.insert(ino, record);
    }
    fn collect_trash(&mut self, ino: u64) {
        if !self.trash.values().any(|i| *i == ino)
            && !self.handles.values().any(|h| h.ino == ino)
            && self.refs.get(&ino).copied().unwrap_or(0) == 0
        {
            self.trash_nodes.remove(&ino);
        }
    }
    fn restore_saved(
        &mut self,
        saved: &crate::model::SavedNode,
        destination: &[u8],
    ) -> FsResult<()> {
        let parts = components(destination)?;
        let mut parent = ROOT;
        for part in &parts[..parts.len() - 1] {
            parent = self.live.lookup(parent, OsStr::from_bytes(part))?;
        }
        self.live
            .restore_file(parent, OsStr::from_bytes(parts.last().unwrap()), saved)?;
        self.dirty = true;
        Ok(())
    }
    fn trash_index_bytes(&self) -> Vec<u8> {
        history::json_bytes(&json!({"schema_version":1,
            "entries":self.trash.values().map(|ino|self.trash_nodes[ino].metadata()).collect::<Vec<_>>()}))
    }
    fn diff_bytes(&self, ino: u64) -> FsResult<Vec<u8>> {
        let view = &self.views[self.diff_nodes.get(&ino).ok_or(libc::ENOENT)?];
        Ok(history::json_bytes(&history::diff(
            &view.saved.name,
            &view.saved.tree,
            &self.live.export(),
        )?))
    }

    fn snapshot_name(name: &str) -> FsResult<()> {
        MemFs::valid_name(OsStr::new(name))
    }

    fn purge_trash(&mut self, id: Option<u64>) -> FsResult<()> {
        if let Some(id) = id {
            if !self.trash.contains_key(&id) {
                return Err(libc::ENOENT);
            }
        }
        let records: Vec<_> = self
            .records()
            .into_iter()
            .filter(|r| id.is_some() && Some(r.id) != id)
            .collect();
        self.commit_with_trash(&self.snapshots(), &records)?;
        let removed: Vec<_> = self
            .trash
            .iter()
            .filter(|(key, _)| id.is_none() || Some(**key) == id)
            .map(|(key, ino)| (*key, *ino))
            .collect();
        for (key, ino) in removed {
            self.trash.remove(&key);
            self.collect_trash(ino);
        }
        Ok(())
    }

    pub fn command(&mut self, command: Command) -> FsResult<()> {
        match command {
            Command::RestoreTree {
                snapshot,
                source,
                destination,
                dry_run,
            } => self
                .restore_tree_output(&snapshot, &source, &destination, dry_run)
                .map(|_| ()),
            Command::Sync => self.sync(),
            Command::TrashRestore { id, destination } => {
                let record = self.trash_nodes[self.trash.get(&id).ok_or(libc::ENOENT)?].saved();
                self.restore_saved(&record, &destination)
            }
            Command::TrashPurge { id } => self.purge_trash(Some(id)),
            Command::TrashPurgeAll => self.purge_trash(None),
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
        let (cache_bytes, fetched_bytes) = self
            .store
            .as_ref()
            .map(|s| {
                let r = s.reader.lock().unwrap();
                (r.cache_bytes(), r.fetched_bytes)
            })
            .unwrap_or((0, 0));
        let mut seen = std::collections::HashSet::new();
        let dirty_bytes = self
            .live
            .contents()
            .iter()
            .chain(
                self.views
                    .values()
                    .flat_map(|v| v.saved.tree.nodes.iter().map(|n| &n.data)),
            )
            .chain(self.trash_nodes.values().map(|r| &r.data))
            .filter(|c| seen.insert(c.key()))
            .map(|c| c.memory_bytes())
            .sum::<usize>();
        let mut value = json!({
            "schema_version":1,"filesystem":"TeamFS","version":env!("CARGO_PKG_VERSION"),"mode":self.mode(),
            "uptime_seconds":self.started.elapsed().as_secs(),"files":files,"directories":directories,
            "used_bytes":self.live.used_bytes(),"capacity_bytes":self.live.capacity,"max_file_bytes":self.live.max_file,
            "snapshot_count":self.active.len(),"snapshot_bytes":self.snapshot_bytes(),"snapshot_limit":MAX_SNAPSHOTS,"snapshot_capacity_bytes":SNAPSHOT_CAPACITY,
            "snapshots":self.active.iter().map(|(name,id)|json!({"name":name,"created_unix":self.views[id].saved.created})).collect::<Vec<_>>(),
            "read_calls":self.read_calls,"write_calls":self.write_calls,"read_bytes":self.read_bytes,"write_bytes":self.write_bytes,
            "dirty":self.dirty,"last_sync_unix":self.last_sync,"last_sync_error":self.last_sync_error,
            "trash_count":self.trash.len(),"trash_bytes":self.trash_bytes(),"trash_limit":TRASH_LIMIT,"trash_capacity_bytes":TRASH_CAPACITY,
            "auto_sync_interval_seconds":self.auto_sync_interval,"auto_sync_attempts":self.auto_sync_attempts,"auto_sync_successes":self.auto_sync_successes,
            "store_path_bytes":self.store.as_ref().map(|s|s.database.as_os_str().as_bytes()),
            "cache_bytes":cache_bytes,"cache_limit_bytes":crate::content::CACHE_LIMIT,"content_fetched_bytes":fetched_bytes,
            "resident_unsaved_content_bytes":dirty_bytes,
            "last_commit_rows":self.store.as_ref().map_or(0,|s|s.last_rows),
            "last_commit_content_bytes":self.store.as_ref().map_or(0,|s|s.last_content_bytes),
            "audit":self.audit.summary()
        });
        value["performance"] = self.metrics.summary();
        value["runtime"] = json!({"open_handles":self.handles.len(),"retired_snapshots":self.views.len()-self.active.len(),"retired_trash":self.trash_nodes.len()-self.trash.len(),"live":self.live.runtime_stats()});
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
            ADMIN => Ok(self.virtual_attr(ino, Kind::Directory, 0o555, 0, 5)),
            TRASH | DIFFS => Ok(self.virtual_attr(ino, Kind::Directory, 0o555, 0, 2)),
            TRASH_INDEX => Ok(self.virtual_attr(
                ino,
                Kind::File,
                0o444,
                self.trash_index_bytes().len() as u64,
                1,
            )),
            SNAPSHOTS => {
                Ok(self.virtual_attr(ino, Kind::Directory, 0o555, 0, 2 + self.active.len() as u32))
            }
            STATUS => {
                Ok(self.virtual_attr(ino, Kind::File, 0o444, self.status_bytes().len() as u64, 1))
            }
            CONTROL => Ok(self.virtual_attr(ino, Kind::File, 0o600, 0, 1)),
            METRICS => {
                Ok(self.virtual_attr(ino, Kind::File, 0o444, self.metrics.bytes().len() as u64, 1))
            }
            EVENTS => {
                Ok(self.virtual_attr(ino, Kind::File, 0o444, self.audit.bytes().len() as u64, 1))
            }
            _ if self.diff_nodes.contains_key(&ino) => Ok(self.virtual_attr(
                ino,
                Kind::File,
                0o444,
                self.diff_bytes(ino)?.len() as u64,
                1,
            )),
            _ if self.trash_nodes.contains_key(&ino) => {
                let mut attr = self.trash_nodes[&ino].attr.clone();
                attr.ino = ino;
                attr.mode = 0o444;
                attr.nlink = u32::from(self.trash.values().any(|i| *i == ino));
                Ok(attr)
            }
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
            SNAPSHOTS | TRASH | DIFFS => ADMIN,
            STATUS | CONTROL | TRASH_INDEX | EVENTS | METRICS => return Err(libc::ENOTDIR),
            _ if self.diff_nodes.contains_key(&ino) || self.trash_nodes.contains_key(&ino) => {
                return Err(libc::ENOTDIR)
            }
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
                entries.push(entry(TRASH, "trash", Kind::Directory));
                entries.push(entry(DIFFS, "diffs", Kind::Directory));
                entries.push(entry(EVENTS, "events.json", Kind::File));
                entries.push(entry(METRICS, "metrics.json", Kind::File));
            }
            TRASH => {
                entries.push(entry(TRASH_INDEX, "index.json", Kind::File));
                for (id, ino) in &self.trash {
                    entries.push(entry(*ino, &id.to_string(), Kind::File));
                }
            }
            DIFFS => {
                for (name, root) in &self.active {
                    entries.push(entry(self.views[root].diff_ino, name, Kind::File));
                }
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
        if let Some(view) = self.diff_nodes.get(&ino).copied() {
            self.collect_view(view);
        }
        self.collect_trash(ino);
    }
    fn collect_view(&mut self, view: u64) {
        if self.active.values().any(|v| *v == view)
            || self.handles.values().any(|h| h.view == Some(view))
            || self.refs.iter().any(|(ino, count)| {
                *count > 0
                    && (self.virtual_nodes.get(ino).map(|v| v.0) == Some(view)
                        || self.diff_nodes.get(ino) == Some(&view))
            })
        {
            return;
        }
        self.views.remove(&view);
        self.virtual_nodes.retain(|_, value| value.0 != view);
        self.diff_nodes.retain(|_, value| *value != view);
    }

    fn add_handle(&mut self, ino: u64, data: HandleData) -> u64 {
        let fh = self.next_fh;
        self.next_fh += 1;
        self.handles.insert(
            fh,
            Handle {
                ino,
                view: self
                    .virtual_nodes
                    .get(&ino)
                    .map(|v| v.0)
                    .or_else(|| self.diff_nodes.get(&ino).copied()),
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
            if ![libc::O_WRONLY, libc::O_RDWR].contains(&(flags & libc::O_ACCMODE)) {
                return Err(libc::EACCES);
            }
            HandleData::Control {
                bytes: Vec::new(),
                outcome: None,
                response: Vec::new(),
            }
        } else {
            if flags & libc::O_ACCMODE != libc::O_RDONLY || flags & libc::O_TRUNC != 0 {
                return Err(libc::EROFS);
            }
            if ino == STATUS {
                HandleData::Frozen(self.status_bytes())
            } else if ino == METRICS {
                HandleData::Frozen(self.metrics.bytes())
            } else if ino == EVENTS {
                HandleData::Frozen(self.audit.bytes())
            } else if ino == TRASH_INDEX {
                HandleData::Frozen(self.trash_index_bytes())
            } else if self.diff_nodes.contains_key(&ino) {
                HandleData::Frozen(self.diff_bytes(ino)?)
            } else if self.trash_nodes.contains_key(&ino) {
                HandleData::Trash
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
        self.collect_trash(ino);
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
                self.views[&view].saved.tree.nodes[index]
                    .data
                    .read(offset as usize, size as usize)?
            }
            HandleData::Frozen(bytes) => return Ok(slice(bytes, offset, size)),
            HandleData::Control {
                response, outcome, ..
            } => match outcome {
                Some(Ok(())) => return Ok(slice(response, offset, size)),
                Some(Err(e)) => return Err(*e),
                None => return Err(libc::EAGAIN),
            },
            HandleData::Trash => self.trash_nodes[&ino]
                .data
                .read(offset as usize, size as usize)?,
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
                ..
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
            HandleData::Control { bytes, outcome, .. } => {
                if let Some(result) = outcome {
                    return *result;
                }
                Some(serde_json::from_slice::<Command>(bytes).map_err(|_| libc::EINVAL))
            }
            _ => None,
        };
        if let Some(parsed) = parsed {
            let began = Instant::now();
            let args = parsed
                .as_ref()
                .ok()
                .and_then(|c| serde_json::to_value(c).ok())
                .unwrap_or(json!({}));
            let operation = args["operation"]
                .as_str()
                .unwrap_or("invalid_control_request")
                .to_owned();
            let commit_before = self.metrics.commit_total_us;
            let previous = self.operation_context.replace(operation.clone());
            let output = parsed.and_then(|command| self.command_output(command));
            self.operation_context = previous;
            let result = output.as_ref().map(|_| ()).map_err(|e| *e);
            let source: Option<Vec<u8>> = serde_json::from_value(args["source"].clone()).ok();
            let destination: Option<Vec<u8>> =
                serde_json::from_value(args["destination"].clone()).ok();
            let duration = began.elapsed().as_micros() as u64;
            self.metrics.record(
                &format!("management.{operation}"),
                destination.as_deref().or(source.as_deref()),
                self.caller.as_ref().map(|c| c.pid),
                result.err(),
                crate::metrics::Timing {
                    total_us: duration,
                    service_us: duration,
                    commit_us: self.metrics.commit_total_us.saturating_sub(commit_before),
                    lock_wait_us: 0,
                },
            );
            self.audit.record(
                "management",
                &operation,
                self.caller.clone(),
                None,
                source,
                destination,
                args,
                result.err(),
                None,
                duration,
                self.dirty,
            );
            if let HandleData::Control {
                outcome, response, ..
            } = &mut self.handles.get_mut(&fh).unwrap().data
            {
                *outcome = Some(result);
                if let Ok(bytes) = output {
                    *response = bytes;
                }
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
        let ino = self.live.check_remove(parent, name, directory)?;
        let record = if directory {
            None
        } else {
            Some(self.prepare_trash(ino, "unlink")?)
        };
        self.live.remove(parent, name, directory)?;
        if let Some((ino, record)) = record {
            self.accept_trash(ino, record);
        }
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
        let target = self.live.check_rename(parent, name, new_parent, new_name)?;
        if parent == new_parent && name == new_name {
            return Ok(());
        }
        let record = match target {
            Some(ino) if self.live.attr(ino)?.kind != Kind::Directory => {
                Some(self.prepare_trash(ino, "rename_replace")?)
            }
            _ => None,
        };
        self.live.rename(parent, name, new_parent, new_name)?;
        if let Some((ino, record)) = record {
            self.accept_trash(ino, record);
        }
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
    fn retired_trash_waits_for_handle_and_lookup_then_releases_bytes() {
        let mut s = Service::new(1000, 1000, None).unwrap();
        s.remove(ROOT, OsStr::new("welcome.txt"), false).unwrap();
        let id = *s.trash.keys().next().unwrap();
        let ino = s.trash[&id];
        s.remember(ino);
        let fh = s.open(ino, libc::O_RDONLY).unwrap();
        s.command(Command::TrashPurge { id }).unwrap();
        assert_eq!(s.trash_bytes(), 0);
        assert!(!s.read(ino, fh, 0, 4096).unwrap().is_empty());
        s.close(ino, fh).unwrap();
        assert!(s.trash_nodes.contains_key(&ino));
        s.forget(ino, 1);
        assert!(!s.trash_nodes.contains_key(&ino));
    }
    #[test]
    fn diff_handle_and_lookup_retain_retired_view_until_both_release() {
        let mut s = Service::new(1000, 1000, None).unwrap();
        s.command(Command::SnapshotCreate {
            name: "base".into(),
        })
        .unwrap();
        let view = s.active["base"];
        let ino = s.views[&view].diff_ino;
        s.remember(ino);
        let fh = s.open(ino, libc::O_RDONLY).unwrap();
        let before = s.read(ino, fh, 0, 10000).unwrap();
        s.command(Command::SnapshotDelete {
            name: "base".into(),
        })
        .unwrap();
        assert_eq!(s.read(ino, fh, 0, 10000).unwrap(), before);
        s.close(ino, fh).unwrap();
        assert!(s.views.contains_key(&view));
        s.forget(ino, 1);
        assert!(!s.views.contains_key(&view));
        assert!(!s.diff_nodes.contains_key(&ino));
    }
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
        service
            .remove(ROOT, OsStr::new("welcome.txt"), false)
            .unwrap();
        let fh = service.open(CONTROL, libc::O_WRONLY).unwrap();
        service
            .write(CONTROL, fh, 0, br#"{"operation":"trash_purge"}"#)
            .unwrap();
        assert_eq!(service.fsync(CONTROL, fh), Err(libc::EINVAL));
        assert_eq!(service.trash.len(), 1);
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
