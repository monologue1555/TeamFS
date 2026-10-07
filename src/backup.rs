//! Independent, checksummed, standalone SQLite backups; imports only publish new directories.
use crate::store::Store;
use rusqlite::{backup::Backup, Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    ffi::CString,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::{ffi::OsStrExt, fs::DirBuilderExt},
    path::{Path, PathBuf},
    time::Duration,
};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub format: u32,
    pub teamfs: String,
    pub created_unix: i64,
    pub database_bytes: u64,
    pub sha256: String,
}
fn hash(path: &Path) -> Result<(u64, String), String> {
    let mut file = File::open(path).map_err(|e| e.to_string())?;
    let mut digest = Sha256::new();
    let mut buf = vec![0u8; 1024 * 1024];
    let mut len = 0;
    loop {
        let n = file.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        digest.update(&buf[..n]);
        len += n as u64;
    }
    Ok((len, format!("{:x}", digest.finalize())))
}
pub fn target_path(path: &Path) -> Result<PathBuf, String> {
    let name = path.file_name().ok_or("必须指定新的目标目录名称")?;
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let result = parent.canonicalize().map_err(|e| e.to_string())?.join(name);
    if result.symlink_metadata().is_ok() {
        return Err("目标已经存在；不会覆盖，请选择新目录".into());
    }
    Ok(result)
}
struct Stage(PathBuf);
impl Drop for Stage {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn stage(target: &Path) -> Result<Stage, String> {
    let root = target.parent().ok_or("missing parent")?;
    for attempt in 0..100 {
        let path = root.join(format!(
            ".teamfs-stage-{}-{}-{attempt}",
            std::process::id(),
            time::get_time().nsec
        ));
        match fs::DirBuilder::new().mode(0o700).create(&path) {
            Ok(()) => return Ok(Stage(path)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.to_string()),
        }
    }
    Err("无法建立临时目录".into())
}
fn publish(stage: &Stage, target: &Path) -> Result<(), String> {
    File::open(&stage.0)
        .and_then(|f| f.sync_all())
        .map_err(|e| e.to_string())?;
    let src = CString::new(stage.0.as_os_str().as_bytes()).map_err(|e| e.to_string())?;
    let dst = CString::new(target.as_os_str().as_bytes()).map_err(|e| e.to_string())?;
    if unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            src.as_ptr(),
            libc::AT_FDCWD,
            dst.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    } != 0
    {
        return Err(std::io::Error::last_os_error().to_string());
    }
    File::open(target.parent().unwrap())
        .and_then(|f| f.sync_all())
        .map_err(|e| e.to_string())
}
pub fn create(database: &Path, destination: &Path) -> Result<Manifest, String> {
    let target = target_path(destination)?;
    if target.starts_with(database.parent().ok_or("missing store parent")?) {
        return Err("备份必须位于原存储目录之外".into());
    }
    let stage = stage(&target)?;
    let db = stage.0.join("state.sqlite3");
    {
        let source = Connection::open_with_flags(database, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|e| e.to_string())?;
        source
            .busy_timeout(Duration::from_secs(2))
            .map_err(|e| e.to_string())?;
        let mut dest = Connection::open(&db).map_err(|e| e.to_string())?;
        Backup::new(&source, &mut dest)
            .map_err(|e| e.to_string())?
            .run_to_completion(128, Duration::from_millis(5), None)
            .map_err(|e| e.to_string())?;
        dest.execute_batch("PRAGMA journal_mode=DELETE; PRAGMA synchronous=FULL;
            DELETE FROM blobs WHERE id NOT IN (SELECT content_id FROM nodes UNION SELECT content_id FROM trash); VACUUM;").map_err(|e|e.to_string())?;
    }
    Store::validate_backup(&db)?;
    let (database_bytes, sha256) = hash(&db)?;
    let manifest = Manifest {
        format: 1,
        teamfs: env!("CARGO_PKG_VERSION").into(),
        created_unix: time::get_time().sec,
        database_bytes,
        sha256,
    };
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(stage.0.join("manifest.json"))
        .map_err(|e| e.to_string())?;
    file.write_all(&serde_json::to_vec_pretty(&manifest).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    file.sync_all().map_err(|e| e.to_string())?;
    File::open(&db)
        .and_then(|f| f.sync_all())
        .map_err(|e| e.to_string())?;
    publish(&stage, &target)?;
    Ok(manifest)
}
fn manifest(path: &Path) -> Result<Manifest, String> {
    let bytes = fs::read(path.join("manifest.json")).map_err(|e| e.to_string())?;
    if bytes.len() > 65536 {
        return Err("备份清单过大".into());
    }
    let m: Manifest = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    if m.format != 1 || m.sha256.len() != 64 {
        return Err("不支持或损坏的备份清单".into());
    }
    Ok(m)
}
fn check(path: &Path, m: &Manifest) -> Result<(), String> {
    let (len, digest) = hash(path)?;
    if len != m.database_bytes || digest != m.sha256 {
        return Err("备份 SHA256 或长度校验失败".into());
    }
    Store::validate_backup(path)?;
    Ok(())
}
pub fn verify(path: &Path) -> Result<Manifest, String> {
    let m = manifest(path)?;
    check(&path.join("state.sqlite3"), &m)?;
    Ok(m)
}
pub fn import(path: &Path, destination: &Path) -> Result<(), String> {
    let m = manifest(path)?;
    let target = target_path(destination)?;
    let stage = stage(&target)?;
    let db = stage.0.join("state.sqlite3");
    fs::copy(path.join("state.sqlite3"), &db).map_err(|e| e.to_string())?;
    // Verify the actual copied bytes, not a source that might change between verification and copy.
    check(&db, &m)?;
    File::open(&db)
        .and_then(|f| f.sync_all())
        .map_err(|e| e.to_string())?;
    publish(&stage, &target)
}
