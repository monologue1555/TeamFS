//! SQLite 只在明确同步点提交完整状态；事务失败不会破坏上一次提交。
use crate::model::{Attr, MemFs, SavedNode, Tree};
use rusqlite::{params, Connection, OpenFlags};
use std::fs::{File, OpenOptions};
use std::os::unix::io::AsRawFd;
use std::path::Path;

pub const MAX_SNAPSHOTS: usize = 10;
pub const SNAPSHOT_CAPACITY: usize = 128 * 1024 * 1024;
pub struct Snapshot {
    pub name: String,
    pub created: i64,
    pub tree: Tree,
}
pub struct Loaded {
    pub tree: Tree,
    pub snapshots: Vec<Snapshot>,
    pub synced: i64,
}
pub struct Store {
    conn: Connection,
    _lock: File,
}

impl Store {
    pub fn open(dir: &Path) -> Result<(Self, Option<Loaded>), String> {
        let lock = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(dir.join("store.lock"))
            .map_err(|e| e.to_string())?;
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err("存储目录已被另一个 TeamFS 进程使用".into());
        }
        let db = dir.join("state.sqlite3");
        let fresh = !db.exists();
        let flags = OpenFlags::SQLITE_OPEN_READ_WRITE
            | if fresh {
                OpenFlags::SQLITE_OPEN_CREATE
            } else {
                OpenFlags::empty()
            };
        let conn = Connection::open_with_flags(&db, flags).map_err(|e| e.to_string())?;
        conn.busy_timeout(std::time::Duration::from_secs(2))
            .map_err(|e| e.to_string())?;
        if fresh {
            conn.execute_batch("BEGIN; CREATE TABLE metadata(version INTEGER NOT NULL, synced INTEGER NOT NULL); INSERT INTO metadata VALUES(1,0);
                CREATE TABLE trees(name TEXT PRIMARY KEY, next_ino INTEGER NOT NULL, created INTEGER NOT NULL);
                CREATE TABLE nodes(scope TEXT NOT NULL, ino INTEGER NOT NULL, parent INTEGER NOT NULL, name BLOB NOT NULL, attr TEXT NOT NULL, data BLOB NOT NULL, PRIMARY KEY(scope,ino)); COMMIT;").map_err(|e| e.to_string())?;
        }
        let version: i64 = conn
            .query_row("SELECT version FROM metadata", [], |r| r.get(0))
            .map_err(|e| format!("存储格式无效：{e}"))?;
        if version != 1 {
            return Err(format!("不支持的存储格式版本：{version}"));
        }
        let integrity: String = conn
            .query_row("PRAGMA quick_check", [], |r| r.get(0))
            .map_err(|e| e.to_string())?;
        if integrity != "ok" {
            return Err(format!("数据库损坏：{integrity}"));
        }
        let mut store = Self { conn, _lock: lock };
        // 已有存储先验证内容，避免把损坏或不支持的文件当成空库初始化。
        let loaded = if fresh { None } else { Some(store.load()?) };
        store
            .conn
            .execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;")
            .map_err(|e| e.to_string())?;
        Ok((store, loaded))
    }

    fn load(&mut self) -> Result<Loaded, String> {
        let synced = self
            .conn
            .query_row("SELECT synced FROM metadata", [], |r| r.get(0))
            .map_err(|e| e.to_string())?;
        let mut statement = self
            .conn
            .prepare("SELECT name,next_ino,created FROM trees ORDER BY name")
            .map_err(|e| e.to_string())?;
        let rows = statement
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, u64>(1)?,
                    r.get::<_, i64>(2)?,
                ))
            })
            .map_err(|e| e.to_string())?;
        let mut current = None;
        let mut snapshots = Vec::new();
        let mut total = 0usize;
        for row in rows {
            let (name, next_ino, created) = row.map_err(|e| e.to_string())?;
            if !name.is_empty() {
                MemFs::valid_name(std::ffi::OsStr::new(&name))
                    .map_err(|_| "invalid snapshot name")?;
            }
            let mut query = self
                .conn
                .prepare("SELECT ino,parent,name,attr,data FROM nodes WHERE scope=? ORDER BY ino")
                .map_err(|e| e.to_string())?;
            let mut nodes = Vec::new();
            let rows = query
                .query_map([&name], |r| {
                    Ok((
                        r.get::<_, u64>(0)?,
                        r.get::<_, u64>(1)?,
                        r.get::<_, Vec<u8>>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, Vec<u8>>(4)?,
                    ))
                })
                .map_err(|e| e.to_string())?;
            for row in rows {
                let (ino, parent, name, attr, data) = row.map_err(|e| e.to_string())?;
                let attr: Attr = serde_json::from_str(&attr).map_err(|e| e.to_string())?;
                if attr.ino != ino {
                    return Err("inode mismatch".into());
                }
                nodes.push(SavedNode {
                    attr,
                    parent,
                    name,
                    data,
                });
            }
            let tree = Tree { next_ino, nodes };
            MemFs::from_tree(&tree)?;
            if name.is_empty() {
                current = Some(tree);
            } else {
                total = total
                    .checked_add(tree.nodes.iter().map(|n| n.data.len()).sum())
                    .ok_or("snapshot size overflow")?;
                snapshots.push(Snapshot {
                    name,
                    created,
                    tree,
                });
                if snapshots.len() > MAX_SNAPSHOTS || total > SNAPSHOT_CAPACITY {
                    return Err("snapshot limits exceeded".into());
                }
            }
        }
        let orphan_rows: i64 = self
            .conn
            .query_row(
                "SELECT count(*) FROM nodes WHERE scope NOT IN (SELECT name FROM trees)",
                [],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())?;
        if orphan_rows != 0 {
            return Err("orphan stored nodes".into());
        }
        Ok(Loaded {
            tree: current.ok_or("missing current tree")?,
            snapshots,
            synced,
        })
    }

    pub fn save(&mut self, tree: &Tree, snapshots: &[Snapshot], synced: i64) -> Result<(), String> {
        let transaction = self.conn.transaction().map_err(|e| e.to_string())?;
        transaction
            .execute_batch("DELETE FROM nodes; DELETE FROM trees;")
            .map_err(|e| e.to_string())?;
        {
            let mut insert_tree = transaction
                .prepare("INSERT INTO trees VALUES(?,?,?)")
                .map_err(|e| e.to_string())?;
            let mut insert_node = transaction
                .prepare("INSERT INTO nodes VALUES(?,?,?,?,?,?)")
                .map_err(|e| e.to_string())?;
            for (name, created, tree) in std::iter::once(("", 0, tree)).chain(
                snapshots
                    .iter()
                    .map(|s| (s.name.as_str(), s.created, &s.tree)),
            ) {
                insert_tree
                    .execute(params![name, tree.next_ino, created])
                    .map_err(|e| e.to_string())?;
                for n in &tree.nodes {
                    let attr = serde_json::to_string(&n.attr).map_err(|e| e.to_string())?;
                    insert_node
                        .execute(params![name, n.attr.ino, n.parent, &n.name, attr, &n.data])
                        .map_err(|e| e.to_string())?;
                }
            }
        }
        transaction
            .execute("UPDATE metadata SET synced=?", [synced])
            .map_err(|e| e.to_string())?;
        transaction.commit().map_err(|e| e.to_string())
    }
}
