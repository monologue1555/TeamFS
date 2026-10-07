//! Immutable file versions. Clean bytes live in SQLite; only bounded read pages are cached.
use rusqlite::{Connection, OpenFlags};
use std::{
    collections::HashMap,
    path::Path,
    sync::{Arc, Mutex},
};

pub const PAGE: usize = 64 * 1024;
pub const CACHE_LIMIT: usize = 8 * 1024 * 1024;
pub struct Reader {
    conn: Connection,
    pages: HashMap<(i64, usize), (u64, Vec<u8>)>,
    clock: u64,
    pub fetched_bytes: u64,
}
impl Reader {
    pub fn open(path: &Path) -> Result<Arc<Mutex<Self>>, String> {
        let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|e| e.to_string())?;
        conn.execute_batch("PRAGMA cache_size=-2048; PRAGMA query_only=ON;")
            .map_err(|e| e.to_string())?;
        Ok(Arc::new(Mutex::new(Self {
            conn,
            pages: HashMap::new(),
            clock: 0,
            fetched_bytes: 0,
        })))
    }
    pub fn cache_bytes(&self) -> usize {
        self.pages.values().map(|(_, b)| b.len()).sum()
    }
    fn read(&mut self, id: i64, size: usize, start: usize, len: usize) -> Result<Vec<u8>, i32> {
        let end = start.saturating_add(len).min(size);
        let mut out = Vec::with_capacity(end.saturating_sub(start));
        let mut pos = start.min(size);
        while pos < end {
            let page = pos / PAGE;
            self.clock += 1;
            if !self.pages.contains_key(&(id, page)) {
                let bytes: Vec<u8> = self
                    .conn
                    .query_row(
                        "SELECT substr(data,?,?) FROM blobs WHERE id=?",
                        rusqlite::params![page * PAGE + 1, PAGE, id],
                        |r| r.get(0),
                    )
                    .map_err(|_| libc::EIO)?;
                if bytes.len() != PAGE.min(size - page * PAGE) {
                    return Err(libc::EIO);
                }
                self.fetched_bytes += bytes.len() as u64;
                while self.pages.len() >= CACHE_LIMIT / PAGE {
                    let key = *self.pages.iter().min_by_key(|(_, v)| v.0).unwrap().0;
                    self.pages.remove(&key);
                }
                self.pages.insert((id, page), (self.clock, bytes));
            }
            let (age, bytes) = self.pages.get_mut(&(id, page)).unwrap();
            *age = self.clock;
            let offset = pos % PAGE;
            let count = (end - pos).min(bytes.len() - offset);
            out.extend_from_slice(&bytes[offset..offset + count]);
            pos += count;
        }
        Ok(out)
    }
}

enum Source {
    Memory(Vec<u8>),
    Disk(i64, Arc<Mutex<Reader>>),
}
#[derive(Clone)]
pub struct Content {
    source: Arc<Mutex<Source>>,
    size: usize,
}
impl std::fmt::Debug for Content {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Content").field("size", &self.size).finish()
    }
}
impl From<Vec<u8>> for Content {
    fn from(bytes: Vec<u8>) -> Self {
        Self {
            size: bytes.len(),
            source: Arc::new(Mutex::new(Source::Memory(bytes))),
        }
    }
}
impl Content {
    pub fn disk(id: i64, size: usize, reader: Arc<Mutex<Reader>>) -> Self {
        Self {
            size,
            source: Arc::new(Mutex::new(Source::Disk(id, reader))),
        }
    }
    pub fn len(&self) -> usize {
        self.size
    }
    pub fn is_empty(&self) -> bool {
        self.size == 0
    }
    pub fn key(&self) -> usize {
        Arc::as_ptr(&self.source) as usize
    }
    pub fn memory_bytes(&self) -> usize {
        match &*self.source.lock().unwrap() {
            Source::Memory(bytes) => bytes.len(),
            _ => 0,
        }
    }
    pub fn id_for(&self, reader: &Arc<Mutex<Reader>>) -> Option<i64> {
        match &*self.source.lock().unwrap() {
            Source::Disk(id, r) if Arc::ptr_eq(reader, r) => Some(*id),
            _ => None,
        }
    }
    pub fn committed(&self, id: i64, reader: Arc<Mutex<Reader>>) {
        *self.source.lock().unwrap() = Source::Disk(id, reader);
    }
    pub fn read(&self, start: usize, len: usize) -> Result<Vec<u8>, i32> {
        let source = self.source.lock().map_err(|_| libc::EIO)?;
        let (id, reader) = match &*source {
            Source::Memory(bytes) => {
                let start = start.min(bytes.len());
                return Ok(bytes[start..start.saturating_add(len).min(bytes.len())].to_vec());
            }
            Source::Disk(id, reader) => (*id, reader.clone()),
        };
        drop(source);
        let result = reader
            .lock()
            .map_err(|_| libc::EIO)?
            .read(id, self.size, start, len);
        result
    }
    pub fn all(&self) -> Result<Vec<u8>, i32> {
        self.read(0, self.size)
    }
    pub fn edit(&mut self, size: usize, write: Option<(usize, &[u8])>) -> Result<(), i32> {
        if Arc::strong_count(&self.source) > 1 || self.memory_bytes() == 0 && self.size > 0 {
            let bytes = if size == 0 { Vec::new() } else { self.all()? };
            *self = bytes.into();
        }
        let mut source = self.source.lock().map_err(|_| libc::EIO)?;
        if matches!(&*source, Source::Disk(_, _)) {
            *source = Source::Memory(Vec::new());
        }
        if let Source::Memory(bytes) = &mut *source {
            bytes.resize(size, 0);
            if let Some((start, input)) = write {
                bytes[start..start + input.len()].copy_from_slice(input);
            }
        }
        self.size = size;
        Ok(())
    }
    pub fn equals(&self, other: &Self) -> Result<bool, i32> {
        if self.len() != other.len() {
            return Ok(false);
        }
        if self.key() == other.key() {
            return Ok(true);
        }
        let disk_key = |content: &Self| {
            let source = content.source.lock().unwrap();
            match &*source {
                Source::Disk(id, reader) => Some((*id, Arc::as_ptr(reader) as usize)),
                _ => None,
            }
        };
        if let Some(key) = disk_key(self) {
            if disk_key(other) == Some(key) {
                return Ok(true);
            }
        }
        for start in (0..self.len()).step_by(PAGE) {
            if self.read(start, PAGE)? != other.read(start, PAGE)? {
                return Ok(false);
            }
        }
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn edits_copy_only_when_a_history_version_is_retained() {
        let mut current: Content = b"draft".to_vec().into();
        let old = current.clone();
        current.edit(5, Some((0, b"FINAL"))).unwrap();
        assert_eq!(old.all().unwrap(), b"draft");
        assert_eq!(current.all().unwrap(), b"FINAL");
        current.edit(2, None).unwrap();
        assert_eq!(current.all().unwrap(), b"FI");
        assert_eq!(old.all().unwrap(), b"draft");
    }
}
