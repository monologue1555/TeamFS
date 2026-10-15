//! Immutable file versions. Clean bytes live in SQLite; only bounded read pages are cached.
use rusqlite::{Connection, OpenFlags};
use std::{
    collections::{HashMap, HashSet},
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
    chunked: bool,
}
impl Reader {
    pub fn open(path: &Path) -> Result<Arc<Mutex<Self>>, String> {
        let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|e| e.to_string())?;
        conn.execute_batch("PRAGMA cache_size=-2048; PRAGMA query_only=ON;")
            .map_err(|e| e.to_string())?;
        let chunked = conn
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='contents'",
                [],
                |r| r.get::<_, i64>(0),
            )
            .map_err(|e| e.to_string())?
            != 0;
        Ok(Arc::new(Mutex::new(Self {
            conn,
            pages: HashMap::new(),
            clock: 0,
            fetched_bytes: 0,
            chunked,
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
                let bytes: Vec<u8> = if self.chunked {
                    self
                    .conn
                    .query_row(
                        "SELECT b.data FROM content_blocks c JOIN blobs b ON b.id=c.blob_id WHERE c.content_id=? AND c.page=?",
                        rusqlite::params![id, page],
                        |r| r.get(0),
                    )
                    .map_err(|_| libc::EIO)?
                } else {
                    self.conn
                        .query_row(
                            "SELECT substr(data,?,?) FROM blobs WHERE id=?",
                            rusqlite::params![page * PAGE + 1, PAGE, id],
                            |r| r.get(0),
                        )
                        .map_err(|_| libc::EIO)?
                };
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

#[derive(Clone)]
enum Source {
    Memory(Vec<u8>),
    Disk(i64, Arc<Mutex<Reader>>),
    Overlay {
        base: Content,
        limit: usize,
        pages: HashMap<usize, Arc<Vec<u8>>>,
    },
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
    pub fn memory_bytes(
        &self,
        sources: &mut HashSet<usize>,
        pages_seen: &mut HashSet<usize>,
    ) -> usize {
        if !sources.insert(self.key()) {
            return 0;
        }
        match &*self.source.lock().unwrap() {
            Source::Memory(bytes) => bytes.len(),
            Source::Overlay { base, pages, .. } => {
                base.memory_bytes(sources, pages_seen)
                    + pages
                        .values()
                        .filter(|b| pages_seen.insert(Arc::as_ptr(b) as usize))
                        .map(|b| b.len())
                        .sum::<usize>()
            }
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
            Source::Overlay { base, limit, pages } => {
                let start = start.min(self.size);
                let end = start.saturating_add(len).min(self.size);
                let mut out = vec![0; end - start];
                if start < *limit {
                    let bytes = base.read(start, end.min(*limit) - start)?;
                    out[..bytes.len()].copy_from_slice(&bytes);
                }
                for page in start / PAGE..(end + PAGE - 1) / PAGE {
                    if let Some(bytes) = pages.get(&page) {
                        let lo = start.max(page * PAGE);
                        let hi = end.min((page + 1) * PAGE);
                        out[lo - start..hi - start]
                            .copy_from_slice(&bytes[lo - page * PAGE..hi - page * PAGE]);
                    }
                }
                return Ok(out);
            }
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
        if let Some((start, input)) = write {
            if start
                .checked_add(input.len())
                .filter(|end| *end <= size)
                .is_none()
            {
                return Err(libc::EINVAL);
            }
        }
        // Prepare on a separate value: read errors must not partially change the version.
        let original = self.source.lock().map_err(|_| libc::EIO)?.clone();
        let mut next = match original {
            Source::Overlay { .. } => original,
            _ => Source::Overlay {
                base: self.clone(),
                limit: self.size,
                pages: HashMap::new(),
            },
        };
        if let Source::Overlay { base, limit, pages } = &mut next {
            if size < self.size {
                *limit = (*limit).min(size);
                pages.retain(|page, _| page * PAGE < size);
                if size % PAGE != 0 {
                    if let Some(bytes) = pages.get_mut(&(size / PAGE)) {
                        Arc::make_mut(bytes)[size % PAGE..].fill(0);
                    }
                }
            }
            if let Some((start, input)) = write {
                let end = start + input.len();
                for page in start / PAGE..(end + PAGE - 1) / PAGE {
                    if !pages.contains_key(&page) {
                        let begin = page * PAGE;
                        let mut bytes = vec![0; PAGE];
                        if begin < *limit {
                            let old = base.read(begin, PAGE.min(*limit - begin))?;
                            bytes[..old.len()].copy_from_slice(&old);
                        }
                        pages.insert(page, Arc::new(bytes));
                    }
                    let lo = start.max(page * PAGE);
                    let hi = end.min((page + 1) * PAGE);
                    Arc::make_mut(pages.get_mut(&page).unwrap())
                        [lo - page * PAGE..hi - page * PAGE]
                        .copy_from_slice(&input[lo - start..hi - start]);
                }
            }
        }
        self.source = Arc::new(Mutex::new(next));
        self.size = size;
        Ok(())
    }
    /// A complete unchanged block can be shared without fetching its bytes.
    pub fn shared_page(&self, page: usize, reader: &Arc<Mutex<Reader>>) -> Option<(i64, usize)> {
        let source = self.source.lock().ok()?;
        match &*source {
            Source::Disk(id, r) if Arc::ptr_eq(reader, r) => Some((*id, page)),
            Source::Overlay { base, limit, pages }
                if ((page + 1) * PAGE).min(self.size) <= *limit
                    && PAGE.min(self.size.saturating_sub(page * PAGE))
                        == PAGE.min(base.size.saturating_sub(page * PAGE)) =>
            {
                if let Some(bytes) = pages.get(&page) {
                    let len = PAGE.min(self.size.saturating_sub(page * PAGE));
                    if base.read(page * PAGE, len).ok()?.as_slice() != &bytes[..len] {
                        return None;
                    }
                }
                base.shared_page(page, reader)
            }
            _ => None,
        }
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
    #[test]
    fn cross_block_edits_and_truncate_growth_preserve_versions() {
        let bytes = vec![7; PAGE * 3 + 13];
        let mut current: Content = bytes.clone().into();
        let old = current.clone();
        current
            .edit(bytes.len(), Some((PAGE - 2, b"abcdef")))
            .unwrap();
        let middle = current.clone();
        assert_eq!(current.read(PAGE - 3, 8).unwrap(), b"\x07abcdef\x07");
        current.edit(PAGE + 1, None).unwrap();
        current.edit(PAGE * 3 + 99, None).unwrap();
        assert!(current
            .read(PAGE + 1, PAGE * 3)
            .unwrap()
            .iter()
            .all(|b| *b == 0));
        assert_eq!(middle.read(PAGE - 2, 6).unwrap(), b"abcdef");
        assert_eq!(old.all().unwrap(), bytes);
        current.edit(0, None).unwrap();
        current.edit(PAGE + 5, Some((PAGE + 3, b"ok"))).unwrap();
        assert!(current.read(0, PAGE + 3).unwrap().iter().all(|b| *b == 0));
        assert_eq!(current.read(PAGE + 3, 2).unwrap(), b"ok");
    }
}
