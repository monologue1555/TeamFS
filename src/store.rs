//! Transactional metadata deltas and immutable payloads; v1/v2 migration is atomic.
use crate::content::{Content, Reader};
use crate::history::{TrashRecord, TRASH_CAPACITY, TRASH_LIMIT};
use crate::model::{Attr, MemFs, SavedNode, Tree, CAPACITY, MAX_FILE_SIZE};
use rusqlite::{params, Connection, OpenFlags};
use std::{
    collections::{HashMap, HashSet},
    fs::{File, OpenOptions},
    os::unix::io::AsRawFd,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

pub const MAX_SNAPSHOTS: usize = 10;
pub const SNAPSHOT_CAPACITY: usize = 128 * 1024 * 1024;
pub const HARD_CAPACITY: usize = 4 * 1024 * 1024 * 1024;
pub const HARD_MAX_FILE: usize = 64 * 1024 * 1024;
#[derive(Clone)]
pub struct Snapshot {
    pub name: String,
    pub created: i64,
    pub tree: Tree,
}
pub struct Loaded {
    pub tree: Tree,
    pub snapshots: Vec<Snapshot>,
    pub synced: i64,
    pub trash: Vec<TrashRecord>,
    pub next_trash_id: u64,
    pub capacity: usize,
    pub max_file: usize,
}
pub struct Store {
    conn: Connection,
    _lock: File,
    pub reader: Arc<Mutex<Reader>>,
    pub database: PathBuf,
    pub last_rows: u64,
    pub last_content_bytes: u64,
}
const BASE:&str="CREATE TABLE metadata(version INTEGER NOT NULL,synced INTEGER NOT NULL,next_trash_id INTEGER NOT NULL,capacity INTEGER NOT NULL,max_file INTEGER NOT NULL);
    INSERT INTO metadata VALUES(3,0,1,67108864,16777216);
    CREATE TABLE trees(name TEXT PRIMARY KEY,next_ino INTEGER NOT NULL,created INTEGER NOT NULL);
    CREATE TABLE nodes(scope TEXT NOT NULL,ino INTEGER NOT NULL,parent INTEGER NOT NULL,name BLOB NOT NULL,attr TEXT NOT NULL,data BLOB NOT NULL,content_id INTEGER,PRIMARY KEY(scope,ino));
    CREATE TABLE trash(id INTEGER PRIMARY KEY,path BLOB NOT NULL,deleted INTEGER NOT NULL,reason TEXT NOT NULL,attr TEXT NOT NULL,data BLOB NOT NULL,content_id INTEGER);
    CREATE TABLE blobs(id INTEGER PRIMARY KEY,data BLOB NOT NULL);";
pub fn valid_limits(capacity: usize, max_file: usize) -> Result<(), String> {
    if capacity == 0
        || capacity > HARD_CAPACITY
        || max_file == 0
        || max_file > HARD_MAX_FILE
        || max_file > capacity
    {
        Err("容量范围：总量 1～4096 MiB，单文件 1～64 MiB，且单文件不超过总量".into())
    } else {
        Ok(())
    }
}
fn version(conn: &Connection) -> Result<i64, String> {
    if conn
        .query_row("SELECT count(*) FROM metadata", [], |r| r.get::<_, i64>(0))
        .map_err(|e| e.to_string())?
        != 1
    {
        return Err("invalid metadata row count".into());
    }
    let v = conn
        .query_row("SELECT version FROM metadata", [], |r| r.get::<_, i64>(0))
        .map_err(|e| e.to_string())?;
    if !(1..=3).contains(&v) {
        return Err(format!("不支持的存储格式版本：{v}"));
    }
    let check: String = conn
        .query_row("PRAGMA quick_check", [], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    if check != "ok" {
        return Err(format!("数据库损坏：{check}"));
    }
    Ok(v)
}

impl Store {
    /// The caller must hold the existing store lock. No migration, GC or initialization.
    pub fn inspect(path: &Path) -> Result<serde_json::Value, String> {
        let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|e| e.to_string())?;
        conn.execute_batch("PRAGMA query_only=ON; BEGIN;")
            .map_err(|e| e.to_string())?;
        let v = version(&conn)?;
        let mut stmt = conn
            .prepare("PRAGMA integrity_check")
            .map_err(|e| e.to_string())?;
        let checks = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(|e| e.to_string())?;
        for check in checks {
            let check = check.map_err(|e| e.to_string())?;
            if check != "ok" {
                return Err(format!("数据库完整性检查失败：{check}"));
            }
        }
        let data = load(&conn, v, Reader::open(path)?)?;
        let node_count = data.tree.nodes.len()
            + data
                .snapshots
                .iter()
                .map(|s| s.tree.nodes.len())
                .sum::<usize>();
        let stored_count: usize = conn
            .query_row("SELECT count(*) FROM nodes", [], |r| r.get(0))
            .map_err(|e| e.to_string())?;
        if node_count != stored_count {
            return Err("存在不属于任何文件树的节点".into());
        }
        let (blobs, blob_bytes, unused, unused_bytes): (u64, u64, u64, u64) = if v == 3 {
            conn.query_row("SELECT count(*),coalesce(sum(length(data)),0),coalesce(sum(CASE WHEN id NOT IN (SELECT content_id FROM nodes UNION SELECT content_id FROM trash) THEN 1 ELSE 0 END),0),coalesce(sum(CASE WHEN id NOT IN (SELECT content_id FROM nodes UNION SELECT content_id FROM trash) THEN length(data) ELSE 0 END),0) FROM blobs",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).map_err(|e|e.to_string())?
        } else {
            (0, 0, 0, 0)
        };
        let scalar = |sql: &str| {
            conn.query_row(sql, [], |r| r.get::<_, u64>(0))
                .map_err(|e| e.to_string())
        };
        let pages = scalar("PRAGMA page_count")?;
        let free = scalar("PRAGMA freelist_count")?;
        let page_size = scalar("PRAGMA page_size")?;
        Ok(
            serde_json::json!({"format":v,"current_nodes":data.tree.nodes.len(),"snapshots":data.snapshots.len(),"trash_records":data.trash.len(),
            "logical_current_bytes":data.tree.nodes.iter().map(|n|n.data.len() as u64).sum::<u64>(),
            "blob_count":blobs,"blob_bytes":blob_bytes,"unreferenced_blobs":unused,"unreferenced_blob_bytes":unused_bytes,
            "database_page_bytes":pages*page_size,"free_page_bytes":free*page_size,
            "note":"SQLite may manage WAL/SHM sidecars while opening read-only; application rows are never changed. No content checksum baseline is stored."}),
        )
    }
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
        let database = dir.join("state.sqlite3");
        let fresh = !database.exists();
        let mut conn = Connection::open_with_flags(
            &database,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | if fresh {
                    OpenFlags::SQLITE_OPEN_CREATE
                } else {
                    OpenFlags::empty()
                },
        )
        .map_err(|e| e.to_string())?;
        conn.busy_timeout(std::time::Duration::from_secs(2))
            .map_err(|e| e.to_string())?;
        if fresh {
            let tx = conn.transaction().map_err(|e| e.to_string())?;
            tx.execute_batch(BASE).map_err(|e| e.to_string())?;
            tx.commit().map_err(|e| e.to_string())?;
        }
        let v = version(&conn)?;
        let reader = Reader::open(&database)?;
        let loaded = if fresh {
            None
        } else {
            Some(load(&conn, v, reader.clone())?)
        };
        conn.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA cache_size=-2048;",
        )
        .map_err(|e| e.to_string())?;
        if v < 3 {
            let tx = conn.transaction().map_err(|e| e.to_string())?;
            if v == 1 {
                tx.execute_batch("CREATE TABLE trash(id INTEGER PRIMARY KEY,path BLOB NOT NULL,deleted INTEGER NOT NULL,reason TEXT NOT NULL,attr TEXT NOT NULL,data BLOB NOT NULL);
                    ALTER TABLE metadata ADD COLUMN next_trash_id INTEGER NOT NULL DEFAULT 1;").map_err(|e|e.to_string())?;
            }
            tx.execute_batch("CREATE TABLE blobs(id INTEGER PRIMARY KEY,data BLOB NOT NULL);
                ALTER TABLE nodes ADD COLUMN content_id INTEGER; ALTER TABLE trash ADD COLUMN content_id INTEGER;
                ALTER TABLE metadata ADD COLUMN capacity INTEGER NOT NULL DEFAULT 67108864;
                ALTER TABLE metadata ADD COLUMN max_file INTEGER NOT NULL DEFAULT 16777216;").map_err(|e|e.to_string())?;
            for table in ["nodes", "trash"] {
                // Old databases have bounded contents. Move one value at a time, transactionally.
                let rows: Vec<(i64, Vec<u8>)> = {
                    let mut stmt = tx
                        .prepare(&format!("SELECT rowid,data FROM {table}"))
                        .map_err(|e| e.to_string())?;
                    let r = stmt
                        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
                        .map_err(|e| e.to_string())?;
                    r.collect::<Result<_, _>>().map_err(|e| e.to_string())?
                };
                for (row, bytes) in rows {
                    tx.execute("INSERT INTO blobs(data) VALUES(?)", [bytes])
                        .map_err(|e| e.to_string())?;
                    tx.execute(
                        &format!("UPDATE {table} SET content_id=?,data=X'' WHERE rowid=?"),
                        params![tx.last_insert_rowid(), row],
                    )
                    .map_err(|e| e.to_string())?;
                }
            }
            tx.execute("UPDATE metadata SET version=3", [])
                .map_err(|e| format!("存储迁移失败：{e}"))?;
            tx.commit().map_err(|e| e.to_string())?;
        }
        let loaded = if v < 3 {
            Some(load(&conn, 3, reader.clone())?)
        } else {
            loaded
        };
        // No runtime handles exist yet. During a mount, retired payloads remain valid until restart.
        conn.execute("DELETE FROM blobs WHERE id NOT IN (SELECT content_id FROM nodes UNION SELECT content_id FROM trash)",[]).map_err(|e|e.to_string())?;
        Ok((
            Self {
                conn,
                _lock: lock,
                reader,
                database,
                last_rows: 0,
                last_content_bytes: 0,
            },
            loaded,
        ))
    }

    pub fn validate_backup(path: &Path) -> Result<Loaded, String> {
        let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|e| e.to_string())?;
        let v = version(&conn)?;
        load(&conn, v, Reader::open(path)?)
    }

    pub fn save(
        &mut self,
        tree: &Tree,
        snapshots: &[Snapshot],
        trash: &[TrashRecord],
        next_trash_id: u64,
        synced: i64,
        capacity: usize,
        max_file: usize,
    ) -> Result<(), String> {
        valid_limits(capacity, max_file)?;
        // Acquire the writer slot before reading comparison rows. A deferred
        // read-to-write upgrade can return BUSY immediately instead of waiting.
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|e| e.to_string())?;
        let mut pending: HashMap<usize, (Content, i64)> = HashMap::new();
        let mut bytes_written = 0;
        let mut rows_written = 0;
        let mut empty_id = tx
            .query_row(
                "SELECT id FROM blobs WHERE length(data)=0 LIMIT 1",
                [],
                |r| r.get::<_, i64>(0),
            )
            .ok();
        let mut content_id = |data: &Content| -> Result<i64, String> {
            if data.is_empty() {
                if let Some(id) = empty_id {
                    return Ok(id);
                }
            }
            if let Some(id) = data.id_for(&self.reader) {
                return Ok(id);
            }
            if let Some((_, id)) = pending.get(&data.key()) {
                return Ok(*id);
            }
            let bytes = data
                .all()
                .map_err(|e| format!("读取待保存内容失败：errno={e}"))?;
            bytes_written += bytes.len() as u64;
            tx.execute("INSERT INTO blobs(data) VALUES(?)", [bytes])
                .map_err(|e| e.to_string())?;
            let id = tx.last_insert_rowid();
            if data.is_empty() {
                empty_id = Some(id);
            }
            pending.insert(data.key(), (data.clone(), id));
            Ok(id)
        };
        let mut old_nodes = HashMap::new();
        {
            let mut q = tx
                .prepare("SELECT scope,ino,parent,name,attr,content_id FROM nodes")
                .map_err(|e| e.to_string())?;
            let rows = q
                .query_map([], |r| {
                    Ok((
                        (r.get::<_, String>(0)?, r.get::<_, u64>(1)?),
                        (
                            r.get::<_, u64>(2)?,
                            r.get::<_, Vec<u8>>(3)?,
                            r.get::<_, String>(4)?,
                            r.get::<_, i64>(5)?,
                        ),
                    ))
                })
                .map_err(|e| e.to_string())?;
            for row in rows {
                let (k, v) = row.map_err(|e| e.to_string())?;
                old_nodes.insert(k, v);
            }
        }
        let mut names = HashSet::new();
        for (name, created, tree) in std::iter::once(("", 0, tree)).chain(
            snapshots
                .iter()
                .map(|s| (s.name.as_str(), s.created, &s.tree)),
        ) {
            names.insert(name.to_string());
            rows_written+=tx.execute("INSERT INTO trees VALUES(?,?,?) ON CONFLICT(name) DO UPDATE SET next_ino=excluded.next_ino,created=excluded.created WHERE next_ino!=excluded.next_ino OR created!=excluded.created",
                params![name,tree.next_ino,created]).map_err(|e|e.to_string())? as u64;
            for n in &tree.nodes {
                let id = content_id(&n.data)?;
                let attr = serde_json::to_string(&n.attr).map_err(|e| e.to_string())?;
                let wanted = (n.parent, n.name.clone(), attr.clone(), id);
                if old_nodes.remove(&(name.to_string(), n.attr.ino)).as_ref() == Some(&wanted) {
                    continue;
                }
                rows_written+=tx.execute("INSERT INTO nodes(scope,ino,parent,name,attr,data,content_id) VALUES(?,?,?,?,?,X'',?) ON CONFLICT(scope,ino) DO UPDATE SET parent=excluded.parent,name=excluded.name,attr=excluded.attr,content_id=excluded.content_id",
                    params![name,n.attr.ino,n.parent,&n.name,attr,id]).map_err(|e|e.to_string())? as u64;
            }
        }
        for ((scope, ino), _) in old_nodes {
            rows_written += tx
                .execute(
                    "DELETE FROM nodes WHERE scope=? AND ino=?",
                    params![scope, ino],
                )
                .map_err(|e| e.to_string())? as u64;
        }
        let old_trees: Vec<String> = {
            let mut q = tx
                .prepare("SELECT name FROM trees")
                .map_err(|e| e.to_string())?;
            let r = q.query_map([], |r| r.get(0)).map_err(|e| e.to_string())?;
            r.collect::<Result<_, _>>().map_err(|e| e.to_string())?
        };
        for name in old_trees {
            if !names.contains(&name) {
                rows_written += tx
                    .execute("DELETE FROM trees WHERE name=?", [name])
                    .map_err(|e| e.to_string())? as u64;
            }
        }
        let mut old_trash = HashMap::new();
        {
            let mut q = tx
                .prepare("SELECT id,path,deleted,reason,attr,content_id FROM trash")
                .map_err(|e| e.to_string())?;
            let rows = q
                .query_map([], |r| {
                    Ok((
                        r.get::<_, u64>(0)?,
                        (
                            r.get::<_, Vec<u8>>(1)?,
                            r.get::<_, i64>(2)?,
                            r.get::<_, String>(3)?,
                            r.get::<_, String>(4)?,
                            r.get::<_, i64>(5)?,
                        ),
                    ))
                })
                .map_err(|e| e.to_string())?;
            for row in rows {
                let (k, v) = row.map_err(|e| e.to_string())?;
                old_trash.insert(k, v);
            }
        }
        for r in trash {
            let id = content_id(&r.data)?;
            let attr = serde_json::to_string(&r.attr).map_err(|e| e.to_string())?;
            let wanted = (
                r.path.clone(),
                r.deleted,
                r.reason.clone(),
                attr.clone(),
                id,
            );
            if old_trash.remove(&r.id).as_ref() == Some(&wanted) {
                continue;
            }
            rows_written+=tx.execute("INSERT INTO trash(id,path,deleted,reason,attr,data,content_id) VALUES(?,?,?,?,?,X'',?) ON CONFLICT(id) DO UPDATE SET path=excluded.path,deleted=excluded.deleted,reason=excluded.reason,attr=excluded.attr,content_id=excluded.content_id",
                params![r.id,&r.path,r.deleted,&r.reason,attr,id]).map_err(|e|e.to_string())? as u64;
        }
        for (id, _) in old_trash {
            rows_written += tx
                .execute("DELETE FROM trash WHERE id=?", [id])
                .map_err(|e| e.to_string())? as u64;
        }
        tx.execute(
            "UPDATE metadata SET synced=?,next_trash_id=?,capacity=?,max_file=?",
            params![synced, next_trash_id, capacity, max_file],
        )
        .map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())?;
        // Only publish disk references after COMMIT; rollback leaves every in-memory version readable.
        for (_, (content, id)) in pending {
            content.committed(id, self.reader.clone());
        }
        self.last_rows = rows_written;
        self.last_content_bytes = bytes_written;
        Ok(())
    }
}

fn load(conn: &Connection, v: i64, reader: Arc<Mutex<Reader>>) -> Result<Loaded, String> {
    let synced = conn
        .query_row("SELECT synced FROM metadata", [], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    let (capacity, max_file) = if v == 3 {
        conn.query_row("SELECT capacity,max_file FROM metadata", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .map_err(|e| e.to_string())?
    } else {
        (CAPACITY, MAX_FILE_SIZE)
    };
    valid_limits(capacity, max_file)?;
    let mut q = conn
        .prepare("SELECT name,next_ino,created FROM trees ORDER BY name")
        .map_err(|e| e.to_string())?;
    let trees = q
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
    let mut snapshot_bytes = 0;
    for row in trees {
        let (name, next_ino, created) = row.map_err(|e| e.to_string())?;
        if !name.is_empty() {
            MemFs::valid_name(std::ffi::OsStr::new(&name)).map_err(|_| "invalid snapshot name")?;
        }
        let sql = if v == 3 {
            "SELECT n.ino,n.parent,n.name,n.attr,n.content_id,length(b.data) FROM nodes n LEFT JOIN blobs b ON b.id=n.content_id WHERE scope=? ORDER BY n.ino"
        } else {
            "SELECT ino,parent,name,attr,data FROM nodes WHERE scope=? ORDER BY ino"
        };
        let mut query = conn.prepare(sql).map_err(|e| e.to_string())?;
        let mut rows = query.query([&name]).map_err(|e| e.to_string())?;
        let mut nodes = Vec::new();
        while let Some(r) = rows.next().map_err(|e| e.to_string())? {
            let ino: u64 = r.get(0).map_err(|e| e.to_string())?;
            let attr: Attr =
                serde_json::from_str(&r.get::<_, String>(3).map_err(|e| e.to_string())?)
                    .map_err(|e| e.to_string())?;
            if attr.ino != ino {
                return Err("inode mismatch".into());
            }
            let data = if v == 3 {
                Content::disk(
                    r.get(4).map_err(|e| e.to_string())?,
                    r.get(5).map_err(|e| e.to_string())?,
                    reader.clone(),
                )
            } else {
                Content::from(r.get::<_, Vec<u8>>(4).map_err(|e| e.to_string())?)
            };
            nodes.push(SavedNode {
                attr,
                parent: r.get(1).map_err(|e| e.to_string())?,
                name: r.get(2).map_err(|e| e.to_string())?,
                data,
            });
        }
        let tree = Tree { next_ino, nodes };
        MemFs::from_tree_limits(
            &tree,
            if name.is_empty() {
                capacity
            } else {
                HARD_CAPACITY
            },
            if name.is_empty() {
                max_file
            } else {
                HARD_MAX_FILE
            },
        )?;
        if name.is_empty() {
            current = Some(tree);
        } else {
            snapshot_bytes += tree.nodes.iter().map(|n| n.data.len()).sum::<usize>();
            snapshots.push(Snapshot {
                name,
                created,
                tree,
            });
            if snapshots.len() > MAX_SNAPSHOTS || snapshot_bytes > SNAPSHOT_CAPACITY {
                return Err("snapshot limits exceeded".into());
            }
        }
    }
    let orphan: i64 = conn
        .query_row(
            "SELECT count(*) FROM nodes WHERE scope NOT IN (SELECT name FROM trees)",
            [],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    if orphan != 0 {
        return Err("orphan stored nodes".into());
    }
    let next_trash_id = if v >= 2 {
        conn.query_row("SELECT next_trash_id FROM metadata", [], |r| {
            r.get::<_, u64>(0)
        })
        .map_err(|e| e.to_string())?
    } else {
        1
    };
    if next_trash_id == 0 || next_trash_id > i64::MAX as u64 {
        return Err("invalid next trash id".into());
    }
    let mut trash = Vec::new();
    let mut total = 0;
    if v >= 2 {
        let sql = if v == 3 {
            "SELECT t.id,t.path,t.deleted,t.reason,t.attr,t.content_id,length(b.data) FROM trash t LEFT JOIN blobs b ON t.content_id=b.id ORDER BY t.id"
        } else {
            "SELECT id,path,deleted,reason,attr,data FROM trash ORDER BY id"
        };
        let mut q = conn.prepare(sql).map_err(|e| e.to_string())?;
        let mut rows = q.query([]).map_err(|e| e.to_string())?;
        while let Some(r) = rows.next().map_err(|e| e.to_string())? {
            let data = if v == 3 {
                Content::disk(
                    r.get(5).map_err(|e| e.to_string())?,
                    r.get(6).map_err(|e| e.to_string())?,
                    reader.clone(),
                )
            } else {
                Content::from(r.get::<_, Vec<u8>>(5).map_err(|e| e.to_string())?)
            };
            let record = TrashRecord {
                id: r.get(0).map_err(|e| e.to_string())?,
                path: r.get(1).map_err(|e| e.to_string())?,
                deleted: r.get(2).map_err(|e| e.to_string())?,
                reason: r.get(3).map_err(|e| e.to_string())?,
                attr: serde_json::from_str(&r.get::<_, String>(4).map_err(|e| e.to_string())?)
                    .map_err(|e| e.to_string())?,
                data,
            };
            record.validate(next_trash_id)?;
            total += record.data.len();
            trash.push(record);
            if trash.len() > TRASH_LIMIT || total > TRASH_CAPACITY {
                return Err("trash capacity exceeded".into());
            }
        }
    }
    Ok(Loaded {
        tree: current.ok_or("missing current tree")?,
        snapshots,
        synced,
        trash,
        next_trash_id,
        capacity,
        max_file,
    })
}
