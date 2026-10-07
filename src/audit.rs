//! Best-effort diagnostics. A slow/full/unwritable log must never reject a business operation.
use serde::Serialize;
use serde_json::{json, Value};
use std::{
    collections::VecDeque,
    fs::{self, File, OpenOptions},
    io::Write,
    os::unix::{ffi::OsStrExt, fs::OpenOptionsExt},
    path::{Path, PathBuf},
    sync::{
        mpsc::{self, SyncSender, TrySendError},
        Arc, Mutex,
    },
    thread,
    time::Duration,
};

const RECENT: usize = 2048;
const LOG_BYTES: u64 = 8 * 1024 * 1024;
#[derive(Clone, Serialize)]
pub struct Caller {
    pub pid: u32,
    pub uid: u32,
    pub gid: u32,
}
#[derive(Clone, Serialize)]
pub struct Event {
    pub schema_version: u32,
    pub session_id: String,
    pub seq: u64,
    pub timestamp_unix_ms: i64,
    pub source: String,
    pub operation: String,
    pub caller: Option<Caller>,
    pub inode: Option<u64>,
    pub path_bytes: Option<Vec<u8>>,
    pub path_display: Option<String>,
    pub destination_bytes: Option<Vec<u8>>,
    pub destination_display: Option<String>,
    pub arguments: Value,
    pub result: String,
    pub errno: Option<i32>,
    pub error_message: Option<String>,
    pub actual_bytes: Option<u64>,
    pub duration_us: u64,
    pub dirty_after: bool,
}
#[derive(Default, Serialize)]
struct FileStatus {
    enabled: bool,
    path_display: Option<String>,
    written: u64,
    dropped: u64,
    last_error: Option<String>,
}
struct Writer {
    sender: Option<SyncSender<String>>,
    finished: mpsc::Receiver<()>,
    thread: Option<thread::JoinHandle<()>>,
    state: Arc<Mutex<FileStatus>>,
}
impl Writer {
    fn new(path: PathBuf) -> Self {
        let (sender, receiver) = mpsc::sync_channel::<String>(1024);
        let (done, finished) = mpsc::channel();
        let state = Arc::new(Mutex::new(FileStatus {
            enabled: true,
            path_display: Some(crate::history::display_path(path.as_os_str().as_bytes())),
            ..Default::default()
        }));
        let shared = state.clone();
        let worker = thread::Builder::new()
            .name("teamfs-audit".into())
            .spawn(move || {
                let mut file: Option<File> = None;
                let mut size = 0;
                while let Ok(line) = receiver.recv() {
                    let result = (|| -> std::io::Result<()> {
                        if file.is_none() {
                            if let Some(parent) = path.parent() {
                                fs::create_dir_all(parent)?;
                            }
                            let f = OpenOptions::new()
                                .create(true)
                                .append(true)
                                .mode(0o600)
                                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                                .open(&path)?;
                            if !f.metadata()?.is_file() {
                                return Err(std::io::Error::new(
                                    std::io::ErrorKind::InvalidInput,
                                    "log is not a regular file",
                                ));
                            }
                            size = f.metadata()?.len();
                            file = Some(f);
                        }
                        if size + line.len() as u64 > LOG_BYTES {
                            file.take();
                            let second = rotated(&path, 2);
                            let first = rotated(&path, 1);
                            match fs::remove_file(&second) {
                                Ok(()) => (),
                                Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
                                Err(e) => return Err(e),
                            }
                            if first.symlink_metadata().is_ok() {
                                fs::rename(&first, &second)?;
                            }
                            fs::rename(&path, &first)?;
                            let f = OpenOptions::new()
                                .create_new(true)
                                .write(true)
                                .mode(0o600)
                                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                                .open(&path)?;
                            file = Some(f);
                            size = 0;
                        }
                        file.as_mut().unwrap().write_all(line.as_bytes())?;
                        size += line.len() as u64;
                        Ok(())
                    })();
                    if let Ok(mut s) = shared.lock() {
                        match result {
                            Ok(()) => {
                                s.written += 1;
                                s.last_error = None;
                            }
                            Err(e) => {
                                s.last_error = Some(e.to_string());
                                s.dropped += 1;
                                file = None;
                            }
                        }
                    }
                }
                // Diagnostic logs are best effort, not part of the filesystem's durability contract.
                if let Some(mut f) = file {
                    let _ = f.flush();
                }
                let _ = done.send(());
            });
        match worker {
            Ok(worker) => Self {
                sender: Some(sender),
                finished,
                thread: Some(worker),
                state,
            },
            Err(e) => {
                state.lock().unwrap().last_error = Some(e.to_string());
                Self {
                    sender: None,
                    finished,
                    thread: None,
                    state,
                }
            }
        }
    }
    fn send(&self, line: String) {
        let result = self.sender.as_ref().map(|s| s.try_send(line));
        if !matches!(result, Some(Ok(()))) {
            if let Ok(mut state) = self.state.lock() {
                state.dropped += 1;
                state.last_error = Some(
                    match result {
                        Some(Err(TrySendError::Full(_))) => {
                            "日志写入暂时跟不上，部分记录未写入文件"
                        }
                        _ => "日志写入线程不可用",
                    }
                    .into(),
                );
            }
        }
    }
}
impl Drop for Writer {
    fn drop(&mut self) {
        self.sender.take();
        if self.finished.recv_timeout(Duration::from_secs(2)).is_ok() {
            if let Some(worker) = self.thread.take() {
                let _ = worker.join();
            }
        }
    }
}
fn rotated(path: &Path, n: u32) -> PathBuf {
    let mut value = path.as_os_str().to_os_string();
    value.push(format!(".{n}"));
    value.into()
}
pub struct Audit {
    pub session_id: String,
    seq: u64,
    recent: VecDeque<Event>,
    writer: Option<Writer>,
    successes: u64,
    errors: u64,
}
impl Audit {
    pub fn new() -> Self {
        let now = time::get_time();
        Self {
            session_id: format!("{}-{}-{}", now.sec, now.nsec, std::process::id()),
            seq: 0,
            recent: VecDeque::new(),
            writer: None,
            successes: 0,
            errors: 0,
        }
    }
    pub fn set_file(&mut self, path: PathBuf) {
        let writer = Writer::new(path);
        for event in &self.recent {
            writer.send(serde_json::to_string(event).unwrap() + "\n");
        }
        self.writer = Some(writer);
    }
    #[allow(clippy::too_many_arguments)]
    pub fn record(
        &mut self,
        source: &str,
        operation: &str,
        caller: Option<Caller>,
        inode: Option<u64>,
        path: Option<Vec<u8>>,
        destination: Option<Vec<u8>>,
        arguments: Value,
        error: Option<i32>,
        bytes: Option<u64>,
        duration_us: u64,
        dirty: bool,
    ) {
        self.seq += 1;
        if error.is_some() {
            self.errors += 1;
        } else {
            self.successes += 1;
        }
        let now = time::get_time();
        let event = Event {
            schema_version: 1,
            session_id: self.session_id.clone(),
            seq: self.seq,
            timestamp_unix_ms: now.sec * 1000 + now.nsec as i64 / 1_000_000,
            source: source.into(),
            operation: operation.into(),
            caller,
            inode,
            path_display: path.as_ref().map(|p| crate::history::display_path(p)),
            path_bytes: path,
            destination_display: destination
                .as_ref()
                .map(|p| crate::history::display_path(p)),
            destination_bytes: destination,
            arguments,
            result: if error.is_some() { "error" } else { "ok" }.into(),
            errno: error,
            error_message: error.map(|e| std::io::Error::from_raw_os_error(e).to_string()),
            actual_bytes: bytes,
            duration_us,
            dirty_after: dirty,
        };
        if let Some(writer) = &self.writer {
            writer.send(serde_json::to_string(&event).unwrap() + "\n");
        }
        self.recent.push_back(event);
        if self.recent.len() > RECENT {
            self.recent.pop_front();
        }
    }
    pub fn summary(&self) -> Value {
        json!({"session_id":self.session_id,"last_seq":self.seq,
            "first_seq":self.recent.front().map(|e|e.seq),"retained":self.recent.len(),
            "evicted":self.seq-self.recent.len() as u64,"successes":self.successes,"errors":self.errors,
            "file":self.writer.as_ref().map(|w|serde_json::to_value(&*w.state.lock().unwrap()).unwrap())})
    }
    pub fn bytes(&self) -> Vec<u8> {
        crate::history::json_bytes(
            &json!({"schema_version":1,"summary":self.summary(),"events":self.recent}),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn recent_window_reports_loss_without_refusing_records() {
        let mut log = Audit::new();
        for _ in 0..RECENT + 3 {
            log.record(
                "test",
                "write",
                None,
                None,
                None,
                None,
                json!({}),
                None,
                Some(1),
                2,
                true,
            );
        }
        let value: Value = serde_json::from_slice(&log.bytes()).unwrap();
        assert_eq!(value["summary"]["evicted"], 3);
        assert_eq!(value["events"][0]["seq"], 4);
        assert_eq!(value["summary"]["successes"], RECENT + 3);
    }
}
