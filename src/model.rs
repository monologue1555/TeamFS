//! 内存模型：独立于 FUSE 回复对象，可以直接用单元测试学习文件系统语义。
//! 名字映射到 inode，inode 映射到节点；打开句柄和路径是两回事。

use crate::content::Content;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::{OsStrExt, OsStringExt};

use libc::{EBADF, EEXIST, EFBIG, EINVAL, EISDIR, ENOENT, ENOTDIR, ENOTEMPTY};
use time::Timespec;

pub type FsResult<T> = Result<T, i32>;
pub const ROOT: u64 = 1;
// 教学实例设定容量上限，防止一次超大偏移写入耗尽内存。按逻辑大小计费。
pub const MAX_FILE_SIZE: usize = 16 * 1024 * 1024;
pub const CAPACITY: usize = 64 * 1024 * 1024;
pub const WELCOME: &str = "欢迎来到 TeamFS！\n这是 Rust + fuse-rs 实现的可恢复资料文件系统。\n试试 ls、cat、mkdir、重定向、mv 和 rm，并观察回调日志。\n运行模式请查看 .teamfs/status.json：persistent 在同步或正常卸载时保存；memory 退出后丢失。\n";
pub const EXAMPLE: &str = "小组笔记示例\n任务：学习 Rust，使用 fuse-rs 实现用户空间文件系统。\n请在 notes 目录里创建自己的笔记。\n";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Kind {
    File,
    Directory,
    Symlink,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Attr {
    pub ino: u64,
    pub size: u64,
    pub kind: Kind,
    pub mode: u16,
    pub uid: u32,
    pub gid: u32,
    pub nlink: u32,
    #[serde(with = "timestamp")]
    pub atime: Timespec,
    #[serde(with = "timestamp")]
    pub mtime: Timespec,
    #[serde(with = "timestamp")]
    pub ctime: Timespec,
    #[serde(with = "timestamp")]
    pub crtime: Timespec,
}

mod timestamp {
    use super::*;
    pub fn serialize<S: serde::Serializer>(t: &Timespec, s: S) -> Result<S::Ok, S::Error> {
        (t.sec, t.nsec).serialize(s)
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Timespec, D::Error> {
        let (sec, nsec) = <(i64, i32)>::deserialize(d)?;
        if !(0..1_000_000_000).contains(&nsec) {
            return Err(serde::de::Error::custom("invalid nanoseconds"));
        }
        Ok(Timespec { sec, nsec })
    }
}

// 持久化只保存有路径的树；句柄、lookup 引用和已 unlink 节点不导出。
#[derive(Clone, Debug)]
pub struct SavedNode {
    pub attr: Attr,
    pub parent: u64,
    pub name: Vec<u8>,
    pub data: Content,
}
#[derive(Clone, Debug)]
pub struct Tree {
    pub next_ino: u64,
    pub nodes: Vec<SavedNode>,
}
pub const VIRTUAL_BASE: u64 = 1 << 60;

enum Data {
    File(Content),
    Symlink(Content),
    Directory(BTreeMap<OsString, u64>),
}

struct Node {
    attr: Attr,
    parent: u64,
    data: Data,
    linked: bool,
    lookup_refs: u64,
}

#[derive(Clone, Debug)]
pub struct DirEntry {
    pub ino: u64,
    pub name: OsString,
    pub kind: Kind,
}

struct Handle {
    ino: u64,
    flags: i32,
    // 打开目录时保存快照，使多次 readdir 的 offset 可稳定续读。
    entries: Option<Vec<DirEntry>>,
}

pub struct MemFs {
    nodes: HashMap<u64, Node>,
    handles: HashMap<u64, Handle>,
    next_ino: u64,
    next_fh: u64,
    pub capacity: usize,
    pub max_file: usize,
}

impl MemFs {
    pub fn runtime_stats(&self) -> serde_json::Value {
        serde_json::json!({"nodes":self.nodes.len(),"open_handles":self.handles.len(),
            "unlinked_nodes":self.nodes.values().filter(|n|!n.linked).count(),
            "kernel_lookup_refs":self.nodes.values().map(|n|n.lookup_refs).sum::<u64>()})
    }
    pub fn new(uid: u32, gid: u32) -> Self {
        let now = time::get_time();
        let root = Node {
            attr: Attr {
                ino: ROOT,
                size: 0,
                kind: Kind::Directory,
                mode: 0o755,
                uid,
                gid,
                nlink: 2,
                atime: now,
                mtime: now,
                ctime: now,
                crtime: now,
            },
            parent: ROOT,
            data: Data::Directory(BTreeMap::new()),
            linked: true,
            lookup_refs: 0,
        };
        let mut fs = Self {
            nodes: HashMap::from([(ROOT, root)]),
            handles: HashMap::new(),
            next_ino: 2,
            next_fh: 1,
            capacity: CAPACITY,
            max_file: MAX_FILE_SIZE,
        };
        let welcome = fs
            .create(ROOT, OsStr::new("welcome.txt"), Kind::File, 0o644, uid, gid)
            .unwrap();
        fs.replace_bytes(welcome, WELCOME.as_bytes());
        let notes = fs
            .create(ROOT, OsStr::new("notes"), Kind::Directory, 0o755, uid, gid)
            .unwrap();
        let example = fs
            .create(
                notes,
                OsStr::new("example.txt"),
                Kind::File,
                0o644,
                uid,
                gid,
            )
            .unwrap();
        fs.replace_bytes(example, EXAMPLE.as_bytes());
        fs
    }

    fn replace_bytes(&mut self, ino: u64, bytes: &[u8]) {
        let node = self.nodes.get_mut(&ino).unwrap();
        node.data = Data::File(bytes.to_vec().into());
        node.attr.size = bytes.len() as u64;
    }

    fn node(&self, ino: u64) -> FsResult<&Node> {
        self.nodes.get(&ino).ok_or(ENOENT)
    }

    fn children(&self, ino: u64) -> FsResult<&BTreeMap<OsString, u64>> {
        match &self.node(ino)?.data {
            Data::Directory(children) => Ok(children),
            Data::File(_) | Data::Symlink(_) => Err(ENOTDIR),
        }
    }

    fn children_mut(&mut self, ino: u64) -> FsResult<&mut BTreeMap<OsString, u64>> {
        match &mut self.nodes.get_mut(&ino).ok_or(ENOENT)?.data {
            Data::Directory(children) => Ok(children),
            Data::File(_) | Data::Symlink(_) => Err(ENOTDIR),
        }
    }

    pub fn valid_name(name: &OsStr) -> FsResult<()> {
        let bytes = name.as_bytes();
        if bytes.is_empty()
            || bytes == b"."
            || bytes == b".."
            || bytes.contains(&b'/')
            || bytes.contains(&0)
        {
            return Err(EINVAL);
        }
        if bytes.len() > 255 {
            return Err(libc::ENAMETOOLONG);
        }
        Ok(())
    }

    fn changed_directory(&mut self, ino: u64) {
        // 目录的 nlink = 自身及父目录的引用（2）+ 直接子目录数量。
        let count = self
            .children(ino)
            .unwrap()
            .values()
            .filter(|child| self.nodes[child].attr.kind == Kind::Directory)
            .count();
        let node = self.nodes.get_mut(&ino).unwrap();
        node.attr.nlink = 2 + count as u32;
        node.attr.mtime = time::get_time();
        node.attr.ctime = node.attr.mtime;
    }

    pub fn attr(&self, ino: u64) -> FsResult<Attr> {
        Ok(self.node(ino)?.attr.clone())
    }

    pub fn lookup(&self, parent: u64, name: &OsStr) -> FsResult<u64> {
        let children = self.children(parent)?;
        if name == OsStr::new(".") {
            return Ok(parent);
        }
        if name == OsStr::new("..") {
            return Ok(self.node(parent)?.parent);
        }
        children.get(name).copied().ok_or(ENOENT)
    }

    pub fn remember(&mut self, ino: u64) {
        if let Some(node) = self.nodes.get_mut(&ino) {
            node.lookup_refs = node.lookup_refs.saturating_add(1);
        }
    }

    pub fn forget(&mut self, ino: u64, count: u64) {
        if let Some(node) = self.nodes.get_mut(&ino) {
            node.lookup_refs = node.lookup_refs.saturating_sub(count);
        }
        self.collect(ino);
    }

    fn collect(&mut self, ino: u64) {
        let unused = self
            .nodes
            .get(&ino)
            .map(|node| !node.linked && node.lookup_refs == 0)
            .unwrap_or(false)
            && !self.handles.values().any(|handle| handle.ino == ino);
        if unused && ino != ROOT {
            self.nodes.remove(&ino);
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
        Self::valid_name(name)?;
        if self.children(parent)?.contains_key(name) {
            return Err(EEXIST);
        }
        if !self.node(parent)?.linked {
            return Err(ENOENT);
        }
        let ino = self.next_ino;
        if ino >= VIRTUAL_BASE - 1 {
            return Err(libc::ENOSPC);
        }
        self.next_ino += 1; // inode 在本次挂载中不复用，避免缓存指向其他文件。
        let now = time::get_time();
        let data = match kind {
            Kind::File => Data::File(Vec::new().into()),
            Kind::Symlink => Data::Symlink(Vec::new().into()),
            Kind::Directory => Data::Directory(BTreeMap::new()),
        };
        self.nodes.insert(
            ino,
            Node {
                attr: Attr {
                    ino,
                    size: 0,
                    kind,
                    mode: (mode & 0o777) as u16,
                    uid,
                    gid,
                    nlink: if kind == Kind::Directory { 2 } else { 1 },
                    atime: now,
                    mtime: now,
                    ctime: now,
                    crtime: now,
                },
                parent,
                data,
                linked: true,
                lookup_refs: 0,
            },
        );
        self.children_mut(parent)?.insert(name.to_os_string(), ino);
        self.changed_directory(parent);
        Ok(ino)
    }

    pub fn entries(&self, ino: u64) -> FsResult<Vec<DirEntry>> {
        let children = self.children(ino)?;
        let mut entries = vec![
            DirEntry {
                ino,
                name: OsString::from("."),
                kind: Kind::Directory,
            },
            DirEntry {
                ino: self.node(ino)?.parent,
                name: OsString::from(".."),
                kind: Kind::Directory,
            },
        ];
        for (name, child) in children {
            entries.push(DirEntry {
                ino: *child,
                name: name.clone(),
                kind: self.node(*child)?.attr.kind,
            });
        }
        Ok(entries)
    }

    pub fn open(&mut self, ino: u64, flags: i32) -> FsResult<u64> {
        if self.node(ino)?.attr.kind != Kind::File {
            return Err(EISDIR);
        }
        let access = flags & libc::O_ACCMODE;
        if ![libc::O_RDONLY, libc::O_WRONLY, libc::O_RDWR].contains(&access) {
            return Err(EINVAL);
        }
        if flags & libc::O_TRUNC != 0 && access != libc::O_RDONLY {
            self.truncate(ino, 0)?;
        }
        Ok(self.add_handle(ino, flags, None))
    }

    pub fn open_dir(&mut self, ino: u64, flags: i32) -> FsResult<u64> {
        let entries = self.entries(ino)?;
        Ok(self.add_handle(ino, flags, Some(entries)))
    }

    fn add_handle(&mut self, ino: u64, flags: i32, entries: Option<Vec<DirEntry>>) -> u64 {
        let fh = self.next_fh;
        self.next_fh += 1;
        self.handles.insert(
            fh,
            Handle {
                ino,
                flags,
                entries,
            },
        );
        fh
    }

    fn handle(&self, ino: u64, fh: u64) -> FsResult<&Handle> {
        self.handles.get(&fh).filter(|h| h.ino == ino).ok_or(EBADF)
    }

    pub fn check_handle(&self, ino: u64, fh: u64) -> FsResult<()> {
        self.handle(ino, fh).map(|_| ())
    }

    pub fn close(&mut self, ino: u64, fh: u64) -> FsResult<()> {
        self.handle(ino, fh)?;
        self.handles.remove(&fh);
        self.collect(ino);
        Ok(())
    }

    pub fn directory_entries(&self, ino: u64, fh: u64) -> FsResult<&[DirEntry]> {
        self.handle(ino, fh)?.entries.as_deref().ok_or(ENOTDIR)
    }

    pub fn read(&mut self, ino: u64, fh: u64, offset: i64, size: u32) -> FsResult<Vec<u8>> {
        if offset < 0 {
            return Err(EINVAL);
        }
        if self.handle(ino, fh)?.flags & libc::O_ACCMODE == libc::O_WRONLY {
            return Err(EBADF);
        }
        let node = self.nodes.get_mut(&ino).ok_or(ENOENT)?;
        match &node.data {
            Data::Directory(_) | Data::Symlink(_) => Err(EISDIR),
            Data::File(bytes) => {
                let start = usize::try_from(offset)
                    .map_err(|_| EINVAL)?
                    .min(bytes.len());
                let end = start.saturating_add(size as usize).min(bytes.len());
                node.attr.atime = time::get_time();
                bytes.read(start, end - start)
            }
        }
    }

    fn check_size(&self, ino: u64, size: usize) -> FsResult<()> {
        if size > self.max_file {
            return Err(EFBIG);
        }
        let old_size = self.node(ino)?.attr.size as usize;
        if self.used_bytes() - old_size + size > self.capacity {
            return Err(libc::ENOSPC);
        }
        Ok(())
    }

    pub fn write(&mut self, ino: u64, fh: u64, offset: i64, bytes: &[u8]) -> FsResult<u32> {
        if offset < 0 {
            return Err(EINVAL);
        }
        let flags = self.handle(ino, fh)?.flags;
        if flags & libc::O_ACCMODE == libc::O_RDONLY {
            return Err(EBADF);
        }
        if self.node(ino)?.attr.kind != Kind::File {
            return Err(EISDIR);
        }
        let start = if flags & libc::O_APPEND != 0 {
            self.node(ino)?.attr.size as usize
        } else {
            usize::try_from(offset).map_err(|_| EFBIG)?
        };
        let end = start.checked_add(bytes.len()).ok_or(EFBIG)?;
        let size = end.max(self.node(ino)?.attr.size as usize);
        if bytes.is_empty() {
            return Ok(0);
        }
        self.check_size(ino, size)?;
        let node = self.nodes.get_mut(&ino).unwrap();
        if let Data::File(content) = &mut node.data {
            content.edit(size, Some((start, bytes)))?;
        }
        node.attr.size = size as u64;
        node.attr.mtime = time::get_time();
        node.attr.ctime = node.attr.mtime;
        Ok(bytes.len() as u32)
    }

    pub fn truncate(&mut self, ino: u64, size: u64) -> FsResult<()> {
        if self.node(ino)?.attr.kind != Kind::File {
            return Err(EISDIR);
        }
        let size = usize::try_from(size).map_err(|_| EFBIG)?;
        self.check_size(ino, size)?;
        let node = self.nodes.get_mut(&ino).unwrap();
        if let Data::File(content) = &mut node.data {
            content.edit(size, None)?;
        }
        node.attr.size = size as u64;
        node.attr.mtime = time::get_time();
        node.attr.ctime = node.attr.mtime;
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
        let node = self.nodes.get_mut(&ino).ok_or(ENOENT)?;
        if let Some(mode) = mode {
            node.attr.mode = (mode & 0o777) as u16;
        }
        if let Some(uid) = uid {
            node.attr.uid = uid;
        }
        if let Some(gid) = gid {
            node.attr.gid = gid;
        }
        if let Some(atime) = atime {
            node.attr.atime = atime;
        }
        if let Some(mtime) = mtime {
            node.attr.mtime = mtime;
        }
        node.attr.ctime = time::get_time();
        Ok(node.attr.clone())
    }

    fn detach(&mut self, parent: u64, name: &OsStr, ino: u64) {
        self.children_mut(parent).unwrap().remove(name);
        let node = self.nodes.get_mut(&ino).unwrap();
        node.linked = false;
        node.attr.nlink = 0;
        node.attr.ctime = time::get_time();
        self.changed_directory(parent);
        // unlink 删除名字；inode 等最后一个打开句柄和内核引用释放后才回收。
        self.collect(ino);
    }

    pub fn check_remove(&self, parent: u64, name: &OsStr, directory: bool) -> FsResult<u64> {
        Self::valid_name(name)?;
        let ino = self.lookup(parent, name)?;
        let kind = self.node(ino)?.attr.kind;
        if directory {
            if kind != Kind::Directory {
                return Err(ENOTDIR);
            }
            if !self.children(ino)?.is_empty() {
                return Err(ENOTEMPTY);
            }
        } else if kind == Kind::Directory {
            return Err(EISDIR);
        }
        Ok(ino)
    }

    pub fn remove(&mut self, parent: u64, name: &OsStr, directory: bool) -> FsResult<()> {
        let ino = self.check_remove(parent, name, directory)?;
        self.detach(parent, name, ino);
        Ok(())
    }

    pub fn check_rename(
        &self,
        parent: u64,
        name: &OsStr,
        new_parent: u64,
        new_name: &OsStr,
    ) -> FsResult<Option<u64>> {
        Self::valid_name(name)?;
        Self::valid_name(new_name)?;
        let ino = self.lookup(parent, name)?;
        self.children(new_parent)?;
        if !self.node(new_parent)?.linked {
            return Err(ENOENT);
        }
        if parent == new_parent && name == new_name {
            return Ok(None);
        }
        let kind = self.node(ino)?.attr.kind;
        if kind == Kind::Directory {
            let mut ancestor = new_parent;
            loop {
                if ancestor == ino {
                    return Err(EINVAL);
                } // 不能把目录搬入自己的子树。
                if ancestor == ROOT {
                    break;
                }
                ancestor = self.node(ancestor)?.parent;
            }
        }
        if let Some(target) = self.children(new_parent)?.get(new_name).copied() {
            let target_kind = self.node(target)?.attr.kind;
            match (kind, target_kind) {
                (Kind::File | Kind::Symlink, Kind::Directory) => return Err(EISDIR),
                (Kind::Directory, Kind::File | Kind::Symlink) => return Err(ENOTDIR),
                (Kind::Directory, Kind::Directory) if !self.children(target)?.is_empty() => {
                    return Err(ENOTEMPTY)
                }
                _ => {}
            }
            return Ok(Some(target));
        }
        Ok(None)
    }

    pub fn rename(
        &mut self,
        parent: u64,
        name: &OsStr,
        new_parent: u64,
        new_name: &OsStr,
    ) -> FsResult<()> {
        let target = self.check_rename(parent, name, new_parent, new_name)?;
        if parent == new_parent && name == new_name {
            return Ok(());
        }
        let ino = self.lookup(parent, name)?;
        if let Some(target) = target {
            self.detach(new_parent, new_name, target);
        }
        self.children_mut(parent)?.remove(name);
        self.children_mut(new_parent)?
            .insert(new_name.to_os_string(), ino);
        let node = self.nodes.get_mut(&ino).unwrap();
        node.parent = new_parent;
        node.attr.ctime = time::get_time();
        self.changed_directory(parent);
        if parent != new_parent {
            self.changed_directory(new_parent);
        }
        Ok(())
    }

    pub fn used_bytes(&self) -> usize {
        self.nodes
            .values()
            .filter(|node| node.attr.kind != Kind::Directory)
            .map(|node| node.attr.size as usize)
            .sum()
    }

    pub fn node_count(&self) -> u64 {
        self.nodes.len() as u64
    }

    /// Called under the service lock, before unlink/replace. No atime or handle changes.
    pub fn saved_file(&self, ino: u64) -> FsResult<(Vec<u8>, SavedNode)> {
        let node = self.node(ino)?;
        let data = match &node.data {
            Data::File(bytes) | Data::Symlink(bytes) => bytes.clone(),
            _ => return Err(EISDIR),
        };
        let mut parts = Vec::new();
        let mut current = ino;
        while current != ROOT {
            let n = self.node(current)?;
            let name = self
                .children(n.parent)?
                .iter()
                .find(|(_, id)| **id == current)
                .ok_or(ENOENT)?
                .0;
            parts.push(name.as_bytes().to_vec());
            current = n.parent;
        }
        let name = parts.first().cloned().ok_or(EINVAL)?;
        parts.reverse();
        Ok((
            parts.join(&b'/'),
            SavedNode {
                attr: node.attr.clone(),
                parent: node.parent,
                name,
                data,
            },
        ))
    }

    pub fn export(&self) -> Tree {
        let mut nodes = Vec::new();
        for (&ino, node) in &self.nodes {
            if !node.linked {
                continue;
            }
            let name = if ino == ROOT {
                Vec::new()
            } else {
                self.children(node.parent)
                    .unwrap()
                    .iter()
                    .find(|(_, child)| **child == ino)
                    .unwrap()
                    .0
                    .as_bytes()
                    .to_vec()
            };
            let data = match &node.data {
                Data::File(bytes) | Data::Symlink(bytes) => bytes.clone(),
                _ => Vec::new().into(),
            };
            nodes.push(SavedNode {
                attr: node.attr.clone(),
                parent: node.parent,
                name,
                data,
            });
        }
        nodes.sort_by_key(|n| n.attr.ino);
        Tree {
            next_ino: self.next_ino,
            nodes,
        }
    }

    #[cfg(test)]
    pub fn from_tree(tree: &Tree) -> Result<Self, String> {
        Self::from_tree_limits(tree, CAPACITY, MAX_FILE_SIZE)
    }
    pub fn from_tree_limits(tree: &Tree, capacity: usize, max_file: usize) -> Result<Self, String> {
        let mut fs = Self {
            nodes: HashMap::new(),
            handles: HashMap::new(),
            next_ino: tree.next_ino,
            next_fh: 1,
            capacity,
            max_file,
        };
        let mut bytes = 0usize;
        for saved in &tree.nodes {
            let a = &saved.attr;
            if a.ino == 0
                || a.ino >= tree.next_ino
                || tree.next_ino >= VIRTUAL_BASE
                || a.mode > 0o777
            {
                return Err("invalid inode/attribute".into());
            }
            let data = match a.kind {
                Kind::File | Kind::Symlink => {
                    if a.size != saved.data.len() as u64 || saved.data.len() > fs.max_file {
                        return Err("invalid file size".into());
                    }
                    bytes = bytes.checked_add(saved.data.len()).ok_or("size overflow")?;
                    if a.kind == Kind::Symlink {
                        Data::Symlink(saved.data.clone())
                    } else {
                        Data::File(saved.data.clone())
                    }
                }
                Kind::Directory => {
                    if !saved.data.is_empty() || a.size != 0 {
                        return Err("invalid directory data".into());
                    }
                    Data::Directory(BTreeMap::new())
                }
            };
            if fs
                .nodes
                .insert(
                    a.ino,
                    Node {
                        attr: a.clone(),
                        parent: saved.parent,
                        data,
                        linked: true,
                        lookup_refs: 0,
                    },
                )
                .is_some()
            {
                return Err("duplicate inode".into());
            }
        }
        if bytes > fs.capacity {
            return Err("content capacity exceeded".into());
        }
        let root = tree
            .nodes
            .iter()
            .find(|n| n.attr.ino == ROOT)
            .ok_or("missing root")?;
        if root.parent != ROOT || !root.name.is_empty() || root.attr.kind != Kind::Directory {
            return Err("invalid root".into());
        }
        for saved in &tree.nodes {
            if saved.attr.ino == ROOT {
                continue;
            }
            let name = OsString::from_vec(saved.name.clone());
            Self::valid_name(&name).map_err(|_| "invalid name")?;
            if saved.parent == ROOT && name == ".teamfs" {
                return Err("reserved name in stored tree".into());
            }
            let children = fs
                .children_mut(saved.parent)
                .map_err(|_| "invalid parent")?;
            if children.insert(name, saved.attr.ino).is_some() {
                return Err("duplicate name".into());
            }
        }
        let mut pending = vec![ROOT];
        let mut seen = std::collections::HashSet::new();
        while let Some(ino) = pending.pop() {
            if !seen.insert(ino) {
                return Err("directory cycle".into());
            }
            let node = &fs.nodes[&ino];
            let nlink = match &node.data {
                Data::File(_) | Data::Symlink(_) => 1,
                Data::Directory(children) => {
                    pending.extend(children.values().copied());
                    2 + children
                        .values()
                        .filter(|i| fs.nodes[i].attr.kind == Kind::Directory)
                        .count() as u32
                }
            };
            if node.attr.nlink != nlink {
                return Err("invalid link count".into());
            }
        }
        if seen.len() != fs.nodes.len() {
            return Err("unreachable nodes".into());
        }
        Ok(fs)
    }

    pub fn counts(&self) -> (usize, usize) {
        (
            self.nodes
                .values()
                .filter(|n| n.linked && n.attr.kind != Kind::Directory)
                .count(),
            self.nodes
                .values()
                .filter(|n| n.linked && n.attr.kind == Kind::Directory)
                .count(),
        )
    }
    pub fn contents(&self) -> Vec<Content> {
        self.nodes
            .values()
            .filter_map(|n| match &n.data {
                Data::File(c) | Data::Symlink(c) => Some(c.clone()),
                _ => None,
            })
            .collect()
    }
    pub fn path_of(&self, ino: u64) -> Option<Vec<u8>> {
        let mut current = ino;
        let mut parts = Vec::new();
        while current != ROOT {
            let node = self.nodes.get(&current)?;
            if !node.linked {
                return None;
            }
            let name = self
                .children(node.parent)
                .ok()?
                .iter()
                .find(|(_, child)| **child == current)?
                .0;
            parts.push(name.as_bytes().to_vec());
            current = node.parent;
        }
        parts.reverse();
        Some(parts.join(&b'/'))
    }

    pub fn restore_file(&mut self, parent: u64, name: &OsStr, saved: &SavedNode) -> FsResult<u64> {
        if saved.attr.kind == Kind::Directory {
            return Err(EISDIR);
        }
        if saved.data.len() > self.max_file {
            return Err(EFBIG);
        }
        if self
            .used_bytes()
            .checked_add(saved.data.len())
            .ok_or(libc::ENOSPC)?
            > self.capacity
        {
            return Err(libc::ENOSPC);
        }
        let ino = self.create(
            parent,
            name,
            saved.attr.kind,
            saved.attr.mode as u32,
            saved.attr.uid,
            saved.attr.gid,
        )?;
        self.nodes.get_mut(&ino).unwrap().data = if saved.attr.kind == Kind::Symlink {
            Data::Symlink(saved.data.clone())
        } else {
            Data::File(saved.data.clone())
        };
        self.nodes.get_mut(&ino).unwrap().attr.size = saved.attr.size;
        let node = self.nodes.get_mut(&ino).unwrap();
        node.attr.mtime = saved.attr.mtime;
        Ok(ino)
    }

    pub fn symlink(
        &mut self,
        parent: u64,
        name: &OsStr,
        target: &[u8],
        uid: u32,
        gid: u32,
    ) -> FsResult<u64> {
        if target.is_empty() || target.contains(&0) || target.len() > 4095 {
            return Err(EINVAL);
        }
        if self.used_bytes() + target.len() > self.capacity {
            return Err(libc::ENOSPC);
        }
        let ino = self.create(parent, name, Kind::Symlink, 0o777, uid, gid)?;
        let n = self.nodes.get_mut(&ino).unwrap();
        n.data = Data::Symlink(target.to_vec().into());
        n.attr.size = target.len() as u64;
        Ok(ino)
    }
    pub fn readlink(&self, ino: u64) -> FsResult<Vec<u8>> {
        match &self.node(ino)?.data {
            Data::Symlink(data) => data.all(),
            _ => Err(EINVAL),
        }
    }
    pub fn configure_limits(&mut self, capacity: usize, max_file: usize) -> Result<(), String> {
        crate::store::valid_limits(capacity, max_file)?;
        if self.used_bytes() > capacity
            || self
                .nodes
                .values()
                .any(|n| n.attr.kind != Kind::Directory && n.attr.size > max_file as u64)
        {
            return Err("新容量低于当前文件实际大小".into());
        }
        self.capacity = capacity;
        self.max_file = max_file;
        Ok(())
    }
    pub fn validate_tree_restore(
        &self,
        parent: u64,
        name: &OsStr,
        nodes: &[SavedNode],
    ) -> FsResult<()> {
        Self::valid_name(name)?;
        if self.children(parent)?.contains_key(name) {
            return Err(EEXIST);
        }
        if !self.node(parent)?.linked {
            return Err(ENOENT);
        }
        if nodes.is_empty() || nodes[0].attr.kind != Kind::Directory {
            return Err(ENOTDIR);
        }
        if nodes
            .iter()
            .any(|n| n.attr.kind != Kind::Directory && n.data.len() > self.max_file)
        {
            return Err(EFBIG);
        }
        let bytes = nodes.iter().map(|n| n.data.len()).sum::<usize>();
        if self.used_bytes().checked_add(bytes).ok_or(libc::ENOSPC)? > self.capacity {
            return Err(libc::ENOSPC);
        }
        if self
            .next_ino
            .checked_add(nodes.len() as u64)
            .ok_or(libc::ENOSPC)?
            >= VIRTUAL_BASE
        {
            return Err(libc::ENOSPC);
        }
        Ok(())
    }
    /// Build an isolated subtree, then attach it once. Existing handles and inode identities survive.
    pub fn restore_tree(&mut self, parent: u64, name: &OsStr, nodes: &[SavedNode]) -> FsResult<()> {
        self.validate_tree_restore(parent, name, nodes)?;
        let ids: HashMap<_, _> = nodes
            .iter()
            .enumerate()
            .map(|(i, n)| (n.attr.ino, self.next_ino + i as u64))
            .collect();
        let root = ids[&nodes[0].attr.ino];
        let mut staged = HashMap::new();
        for (i, saved) in nodes.iter().enumerate() {
            let mut a = saved.attr.clone();
            a.ino = ids[&a.ino];
            a.ctime = time::get_time();
            a.crtime = a.ctime;
            let data = match a.kind {
                Kind::File => Data::File(saved.data.clone()),
                Kind::Symlink => Data::Symlink(saved.data.clone()),
                Kind::Directory => Data::Directory(BTreeMap::new()),
            };
            staged.insert(
                a.ino,
                Node {
                    attr: a,
                    parent: if i == 0 { parent } else { ids[&saved.parent] },
                    data,
                    linked: true,
                    lookup_refs: 0,
                },
            );
        }
        for saved in nodes.iter().skip(1) {
            if let Data::Directory(children) =
                &mut staged.get_mut(&ids[&saved.parent]).unwrap().data
            {
                children.insert(
                    OsStr::from_bytes(&saved.name).to_os_string(),
                    ids[&saved.attr.ino],
                );
            }
        }
        self.nodes.extend(staged);
        self.next_ino += nodes.len() as u64;
        self.children_mut(parent)
            .unwrap()
            .insert(name.to_os_string(), root);
        self.changed_directory(parent);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fs() -> MemFs {
        MemFs::new(1000, 1000)
    }
    fn file(fs: &mut MemFs, name: &str) -> (u64, u64) {
        let ino = fs
            .create(ROOT, OsStr::new(name), Kind::File, 0o644, 1000, 1000)
            .unwrap();
        let fh = fs.open(ino, libc::O_RDWR).unwrap();
        (ino, fh)
    }

    #[test]
    fn seed_and_fresh_mount() {
        let mut a = fs();
        let ino = a.lookup(ROOT, OsStr::new("welcome.txt")).unwrap();
        let fh = a.open(ino, libc::O_RDONLY).unwrap();
        assert_eq!(a.read(ino, fh, 0, 4096).unwrap(), WELCOME.as_bytes());
        file(&mut a, "temporary");
        assert_eq!(fs().lookup(ROOT, OsStr::new("temporary")), Err(ENOENT));
    }

    #[test]
    fn offsets_eof_and_sparse_writes() {
        let mut fs = fs();
        let (ino, fh) = file(&mut fs, "bytes");
        fs.write(ino, fh, 0, b"abc").unwrap();
        fs.write(ino, fh, 1, b"Z").unwrap();
        assert_eq!(fs.read(ino, fh, 0, 99).unwrap(), b"aZc");
        assert_eq!(fs.read(ino, fh, 1, 1).unwrap(), b"Z");
        assert!(fs.read(ino, fh, 99, 99).unwrap().is_empty());
        fs.write(ino, fh, 5, b"!").unwrap();
        assert_eq!(fs.read(ino, fh, 0, 99).unwrap(), b"aZc\0\0!");
        assert_eq!(fs.write(ino, fh, -1, b"x"), Err(EINVAL));
        assert_eq!(fs.read(ino, fh, -1, 1), Err(EINVAL));
        assert_eq!(fs.write(ino, fh, i64::MAX, b"x"), Err(EFBIG));
    }

    #[test]
    fn truncate_and_append() {
        let mut fs = fs();
        let (ino, fh) = file(&mut fs, "笔记.txt");
        fs.write(ino, fh, 0, b"long text").unwrap();
        fs.truncate(ino, 2).unwrap();
        assert_eq!(fs.read(ino, fh, 0, 99).unwrap(), b"lo");
        fs.truncate(ino, 4).unwrap();
        assert_eq!(fs.read(ino, fh, 0, 99).unwrap(), b"lo\0\0");
        let append = fs.open(ino, libc::O_WRONLY | libc::O_APPEND).unwrap();
        fs.write(ino, append, 0, b"!").unwrap();
        assert_eq!(fs.read(ino, fh, 0, 99).unwrap(), b"lo\0\0!");
        fs.open(ino, libc::O_WRONLY | libc::O_TRUNC).unwrap();
        assert_eq!(fs.attr(ino).unwrap().size, 0);
    }

    #[test]
    fn directories_errors_and_snapshot() {
        let mut fs = fs();
        let dir = fs
            .create(ROOT, OsStr::new("目录"), Kind::Directory, 0o755, 1000, 1000)
            .unwrap();
        let child = fs
            .create(dir, OsStr::new("文件"), Kind::File, 0o644, 1000, 1000)
            .unwrap();
        assert_eq!(fs.lookup(dir, OsStr::new("..")), Ok(ROOT));
        assert_eq!(
            fs.create(dir, OsStr::new("文件"), Kind::File, 0, 0, 0),
            Err(EEXIST)
        );
        assert_eq!(fs.remove(ROOT, OsStr::new("目录"), true), Err(ENOTEMPTY));
        assert_eq!(fs.remove(ROOT, OsStr::new("目录"), false), Err(EISDIR));
        assert_eq!(fs.remove(dir, OsStr::new("文件"), true), Err(ENOTDIR));
        assert_eq!(fs.entries(child).unwrap_err(), ENOTDIR);
        let fh = fs.open_dir(dir, 0).unwrap();
        fs.create(dir, OsStr::new("later"), Kind::File, 0, 0, 0)
            .unwrap();
        assert_eq!(fs.directory_entries(dir, fh).unwrap().len(), 3);
        fs.remove(dir, OsStr::new("文件"), false).unwrap();
        fs.remove(dir, OsStr::new("later"), false).unwrap();
        fs.remove(ROOT, OsStr::new("目录"), true).unwrap();
        fs.close(dir, fh).unwrap();
    }

    #[test]
    fn rename_replace_and_cycle() {
        let mut fs = fs();
        let (a, ah) = file(&mut fs, "a");
        let (b, bh) = file(&mut fs, "b");
        fs.write(a, ah, 0, b"new").unwrap();
        fs.write(b, bh, 0, b"old").unwrap();
        fs.rename(ROOT, OsStr::new("a"), ROOT, OsStr::new("b"))
            .unwrap();
        assert_eq!(fs.lookup(ROOT, OsStr::new("b")), Ok(a));
        assert_eq!(fs.read(b, bh, 0, 99).unwrap(), b"old");
        assert_eq!(fs.lookup(ROOT, OsStr::new("a")), Err(ENOENT));
        let dir = fs
            .create(ROOT, OsStr::new("d"), Kind::Directory, 0o755, 0, 0)
            .unwrap();
        let sub = fs
            .create(dir, OsStr::new("s"), Kind::Directory, 0o755, 0, 0)
            .unwrap();
        assert_eq!(
            fs.rename(ROOT, OsStr::new("d"), sub, OsStr::new("cycle")),
            Err(EINVAL)
        );
        fs.rename(ROOT, OsStr::new("b"), dir, OsStr::new("移动.txt"))
            .unwrap();
        assert_eq!(fs.lookup(dir, OsStr::new("移动.txt")), Ok(a));
    }

    #[test]
    fn unlink_open_file_and_kernel_reference() {
        let mut fs = fs();
        let (ino, fh) = file(&mut fs, "open");
        fs.remember(ino);
        fs.write(ino, fh, 0, b"alive").unwrap();
        fs.remove(ROOT, OsStr::new("open"), false).unwrap();
        assert_eq!(fs.lookup(ROOT, OsStr::new("open")), Err(ENOENT));
        assert_eq!(fs.attr(ino).unwrap().nlink, 0);
        assert_eq!(fs.read(ino, fh, 0, 99).unwrap(), b"alive");
        fs.close(ino, fh).unwrap();
        assert!(fs.attr(ino).is_ok());
        fs.forget(ino, 1);
        assert_eq!(fs.attr(ino).unwrap_err(), ENOENT);
    }

    #[test]
    fn access_handles_and_limits() {
        let mut fs = fs();
        let (ino, _) = file(&mut fs, "f");
        let ro = fs.open(ino, libc::O_RDONLY).unwrap();
        let wo = fs.open(ino, libc::O_WRONLY).unwrap();
        assert_eq!(fs.write(ino, ro, 0, b"x"), Err(EBADF));
        assert_eq!(fs.read(ino, wo, 0, 1), Err(EBADF));
        assert_eq!(fs.read(ino, 999, 0, 1), Err(EBADF));
        assert_eq!(fs.truncate(ino, (MAX_FILE_SIZE + 1) as u64), Err(EFBIG));
        assert_eq!(
            fs.create(ROOT, OsStr::new(".."), Kind::File, 0, 0, 0),
            Err(EINVAL)
        );
        assert_eq!(
            fs.create(ino, OsStr::new("child"), Kind::File, 0, 0, 0),
            Err(ENOTDIR)
        );
        for n in 0..3 {
            let (i, _) = file(&mut fs, &format!("large-{n}"));
            fs.truncate(i, MAX_FILE_SIZE as u64).unwrap();
        }
        assert_eq!(fs.truncate(ino, MAX_FILE_SIZE as u64), Err(libc::ENOSPC));
    }

    #[test]
    fn persisted_tree_preserves_raw_names_and_excludes_orphans() {
        let mut original = fs();
        let name = OsString::from_vec(b"raw-\xff".to_vec());
        let ino = original
            .create(ROOT, &name, Kind::File, 0o600, 1000, 1000)
            .unwrap();
        let fh = original.open(ino, libc::O_RDWR).unwrap();
        original.write(ino, fh, 0, b"\0\xff").unwrap();
        let (orphan, handle) = file(&mut original, "orphan");
        original.remove(ROOT, OsStr::new("orphan"), false).unwrap();
        assert!(original.attr(orphan).is_ok());
        let tree = original.export();
        assert!(!tree.nodes.iter().any(|n| n.attr.ino == orphan));
        let mut restored = MemFs::from_tree(&tree).unwrap();
        assert_eq!(restored.lookup(ROOT, &name), Ok(ino));
        let opened = restored.open(ino, libc::O_RDONLY).unwrap();
        assert_eq!(restored.read(ino, opened, 0, 99).unwrap(), b"\0\xff");
        original.close(orphan, handle).unwrap();
    }

    #[test]
    fn persisted_tree_rejects_invalid_relationships() {
        let tree = fs().export();
        let mut broken = tree.clone();
        broken.nodes[1].parent = 999;
        assert!(MemFs::from_tree(&broken).is_err());
        let mut broken = tree.clone();
        broken.nodes[1].name = b".teamfs".to_vec();
        assert!(MemFs::from_tree(&broken).is_err());
        let mut broken = tree.clone();
        broken.nodes[1].attr.size += 1;
        assert!(MemFs::from_tree(&broken).is_err());
        let mut broken = tree;
        broken.nodes.push(broken.nodes[1].clone());
        assert!(MemFs::from_tree(&broken).is_err());
    }
}
