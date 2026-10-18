//! Pinned input versions and run-scoped evidence, independent of best-effort audit logs.
use crate::model::{FsResult, Kind, MemFs, SavedNode, Tree, ROOT};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::{
    ffi::{OsStrExt, OsStringExt},
    fs::{DirBuilderExt, OpenOptionsExt},
    io::{AsRawFd, FromRawFd},
    process::{CommandExt, ExitStatusExt},
};
use std::path::{Path, PathBuf};
use std::process::{Command as Process, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const CHECKPOINT_INTERVAL: Duration = Duration::from_secs(1);

pub const PREFIX: &str = "experiment.";
pub fn link_may_escape(tree: &Tree, index: usize) -> FsResult<bool> {
    // Resolve against the immutable tree, including intermediate symlinks:
    // lexical ../ counting alone cannot detect escapes through a link chain.
    let mut ancestors = Vec::new();
    let mut parent = tree.nodes[index].parent;
    loop {
        ancestors.push(parent);
        if parent == ROOT {
            break;
        }
        parent = tree
            .nodes
            .iter()
            .find(|n| n.attr.ino == parent)
            .ok_or(libc::EIO)?
            .parent;
    }
    ancestors.reverse();
    let target = tree.nodes[index].data.all()?;
    if target.starts_with(b"/") {
        return Ok(true);
    }
    let mut pending: std::collections::VecDeque<Vec<u8>> =
        target.split(|b| *b == b'/').map(|s| s.to_vec()).collect();
    let mut links = 1;
    while let Some(part) = pending.pop_front() {
        match part.as_slice() {
            b"" | b"." => continue,
            b".." => {
                if ancestors.len() == 1 {
                    return Ok(true);
                }
                ancestors.pop();
                continue;
            }
            _ => (),
        }
        let node = match tree.nodes.iter().find(|n| {
            n.attr.ino != ROOT && n.parent == *ancestors.last().unwrap() && n.name == part
        }) {
            Some(node) => node,
            None => return Ok(false), // Missing in-tree target fails normally.
        };
        match node.attr.kind {
            Kind::Directory => ancestors.push(node.attr.ino),
            Kind::File => return Ok(false),
            Kind::Symlink => {
                links += 1;
                if links > 40 {
                    return Ok(true);
                } // Unresolvable cycle is never certified.
                let next = node.data.all()?;
                if next.starts_with(b"/") {
                    return Ok(true);
                }
                for part in next.split(|b| *b == b'/').rev() {
                    pending.push_front(part.to_vec());
                }
            }
        }
    }
    Ok(false)
}
pub fn snapshot_name(name: &str) -> FsResult<String> {
    MemFs::valid_name(OsStr::new(name))?;
    let result = format!("{PREFIX}{name}");
    MemFs::valid_name(OsStr::new(&result))?;
    Ok(result)
}
#[derive(Default)]
pub struct Access {
    pub operations: BTreeMap<String, u64>,
    pub pids: BTreeSet<u32>,
    pub read_calls: u64,
    pub read_bytes: u64,
}
/// Content requests from the group on mount files outside its pinned snapshot.
#[derive(Default)]
pub struct Unpinned {
    pub operations: BTreeMap<String, u64>,
    pub read_bytes: u64,
}
/// Distinct process IDs kept for requests from other process groups; the count stays exact.
pub const OTHER_GROUP_PIDS: usize = 16;
pub struct Session {
    pub snapshot: String,
    pub pgid: u32,
    pub started: i64,
    pub accesses: BTreeMap<usize, Access>,
    pub errors: BTreeMap<(Vec<u8>, String, i32), u64>,
    pub unpinned_accesses: u64,
    /// Keyed by location (business/snapshot/trash) and the path at access time, if any.
    pub unpinned: BTreeMap<(&'static str, Option<Vec<u8>>), Unpinned>,
    /// Pinned snapshot content requested by processes outside the group; never attributed.
    pub other_group_accesses: u64,
    pub other_group_pids: BTreeSet<u32>,
    threads: std::collections::HashMap<u32, u32>,
    pub finished: Option<Value>,
}
impl Session {
    pub fn new(snapshot: String, pgid: u32) -> Self {
        Self {
            snapshot,
            pgid,
            started: time::get_time().sec,
            accesses: BTreeMap::new(),
            errors: BTreeMap::new(),
            unpinned_accesses: 0,
            unpinned: BTreeMap::new(),
            other_group_accesses: 0,
            other_group_pids: BTreeSet::new(),
            threads: std::collections::HashMap::new(),
            finished: None,
        }
    }
    /// Process ID of a calling thread, cached for the session's lifetime.
    pub fn process(&mut self, tid: u32) -> u32 {
        *self.threads.entry(tid).or_insert_with(|| process_id(tid))
    }
}
/// FUSE request headers carry the calling thread ID; map it to its thread group (process).
/// Falls back to the given ID when /proc no longer has the task.
pub fn process_id(tid: u32) -> u32 {
    std::fs::read_to_string(format!("/proc/{tid}/status"))
        .ok()
        .and_then(|status| {
            status
                .lines()
                .find_map(|line| line.strip_prefix("Tgid:")?.trim().parse().ok())
        })
        .unwrap_or(tid)
}
pub fn digest(node: &SavedNode) -> FsResult<String> {
    let mut hash = Sha256::new();
    for offset in (0..node.data.len()).step_by(crate::content::PAGE) {
        hash.update(node.data.read(offset, crate::content::PAGE)?);
    }
    Ok(format!("{:x}", hash.finalize()))
}

/// Identify the immutable input tree by its contents and saved metadata, rather
/// than its reusable display name. This survives backup/import without a schema change.
pub fn input_tree_digest(tree: &Tree) -> FsResult<String> {
    let mut hash = Sha256::new();
    hash.update(b"TeamFS input tree v1\0");
    hash.update(tree.next_ino.to_le_bytes());
    let mut nodes: Vec<_> = tree.nodes.iter().collect();
    nodes.sort_by_key(|node| node.attr.ino);
    for node in nodes {
        let metadata =
            serde_json::to_vec(&(&node.attr, node.parent, &node.name)).map_err(|_| libc::EIO)?;
        hash.update((metadata.len() as u64).to_le_bytes());
        hash.update(metadata);
        hash.update(digest(node)?.as_bytes());
    }
    Ok(format!("{:x}", hash.finalize()))
}

fn request(mount: &OsStr, command: crate::service::Command) -> Result<Value, String> {
    crate::status(mount)?;
    let mut control = OpenOptions::new()
        .read(true)
        .write(true)
        .open(Path::new(mount).join(".teamfs/control"))
        .map_err(|e| e.to_string())?;
    control
        .write_all(&serde_json::to_vec(&command).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    control.sync_all().map_err(|e| management_error(&e))?;
    control
        .seek(SeekFrom::Start(0))
        .map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    control.read_to_end(&mut bytes).map_err(|e| e.to_string())?;
    if bytes.is_empty() {
        Ok(Value::Null)
    } else {
        serde_json::from_slice(&bytes).map_err(|e| e.to_string())
    }
}
/// errno from the control request, with the experiment-specific reason it usually means.
fn management_error(error: &std::io::Error) -> String {
    let hint = match error.raw_os_error() {
        Some(libc::ENOSPC) => format!(
            "；快照条数（含实验输入，最多 {} 个）或历史内容（最多 {} MiB）已满，请先用 experiment delete 清理不再需要的实验输入",
            crate::store::MAX_SNAPSHOTS,
            crate::store::SNAPSHOT_CAPACITY / (1024 * 1024)
        ),
        Some(libc::EEXIST) => "；同名实验输入快照已存在，请换一个实验名称".into(),
        Some(libc::EBUSY) => "；该实验或进程组仍在采集中".into(),
        Some(libc::ENOENT) => "；挂载中没有这个实验的输入快照或采集会话".into(),
        Some(libc::EIO) => "；后端保存失败，请查看 .teamfs/status.json 的 last_sync_error".into(),
        _ => String::new(),
    };
    format!("实验管理请求失败：{error}{hint}")
}
/// Capture conditions that limit what the report may be used for, in plain language.
/// Unpinned and escaping-link reads block verify/replay; other-group requests are a notice.
fn capture_warnings(capture: &Value) -> Vec<(bool, String)> {
    let mut warnings = Vec::new();
    let count = |key: &str| capture[key].as_u64().unwrap_or(0);
    if count("unpinned_mount_accesses") > 0 {
        let files: Vec<String> = capture["unpinned_mount_files"]
            .as_array()
            .map(|rows| {
                rows.iter()
                    .map(|row| {
                        format!(
                            "{}:{}",
                            row["location"].as_str().unwrap_or("?"),
                            row["path_display"].as_str().unwrap_or("<已不在原路径>")
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        warnings.push((
            true,
            format!(
                "打开或读取了 {} 次本实验输入快照之外的挂载内文件{}；这些内容没有固定",
                count("unpinned_mount_accesses"),
                if files.is_empty() {
                    String::new()
                } else {
                    format!("（{}）", files.join("，"))
                }
            ),
        ));
    }
    if count("external_link_accesses") > 0 {
        let links: Vec<&str> = capture["metadata_accesses"]
            .as_array()
            .map(|rows| {
                rows.iter()
                    .filter(|row| row["link_may_escape_input"] == true)
                    .filter_map(|row| row["path_display"].as_str())
                    .collect()
            })
            .unwrap_or_default();
        warnings.push((
            true,
            format!(
                "经由可能离开输入目录的软链接访问 {} 次（{}）；链接目标没有固定",
                count("external_link_accesses"),
                links.join("，")
            ),
        ));
    }
    if count("other_group_snapshot_accesses") > 0 {
        warnings.push((
            false,
            format!(
                "运行期间有本进程组之外的进程打开或读取同一输入快照 {} 次（进程 {}），未计入本实验；若其中有离开进程组的实验进程，本次采集不完整",
                count("other_group_snapshot_accesses"),
                capture["other_group_pids"]
            ),
        ));
    }
    warnings
}
fn save_report(dir: &Path, report: &Value) -> Result<(), String> {
    let temporary = dir.join(format!(
        ".manifest-{}-{}.tmp",
        std::process::id(),
        time::get_time().nsec
    ));
    let result = (|| {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&temporary)
            .map_err(|e| e.to_string())?;
        file.write_all(&crate::history::json_bytes(report))
            .map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
        std::fs::rename(&temporary, dir.join("manifest.json")).map_err(|e| e.to_string())?;
        File::open(dir)
            .and_then(|f| f.sync_all())
            .map_err(|e| e.to_string())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(temporary);
    }
    result
}

fn report_daemon(report: &Value) -> Option<String> {
    report["daemon_session"].as_str().map(str::to_owned)
}

/// Publish only after a complete summary has been fetched and atomically saved.
/// The previous on-disk summary remains usable if polling or publication fails.
fn checkpoint(mount: &OsStr, name: &str, dir: &Path, report: &mut Value) -> Result<(), String> {
    let capture = request(
        mount,
        crate::service::Command::ExperimentCheckpoint {
            name: name.into(),
            daemon_session: report_daemon(report).ok_or("缺少 daemon 会话标识")?,
        },
    )?;
    let mut next = report.clone();
    next["capture"] = capture;
    next["checkpoint_sequence"] = json!(report["checkpoint_sequence"].as_u64().unwrap_or(0) + 1);
    next["checkpoint_unix"] = json!(time::get_time().sec);
    next["capture_source"] = json!("checkpoint");
    save_report(dir, &next)?;
    *report = next;
    Ok(())
}

fn mark_incomplete(report: &mut Value, outcome: &str, reason: &str) {
    report["phase"] = json!("interrupted");
    report["outcome"] = json!(outcome);
    report["capture_complete"] = json!(false);
    report["capture_error"] = json!(reason);
    report["unknown_after_checkpoint"] = json!(report["capture_source"] == "checkpoint");
    report["recovered_unix"] = json!(time::get_time().sec);
    // Exit code/signal are retained only if the runner actually observed them.
}

fn valid_checkpoint(report: &Value) -> bool {
    report["checkpoint_sequence"].as_u64().unwrap_or(0) > 0
        && report["process_group"]
            .as_u64()
            .map_or(false, |id| id > 0 && id <= i32::MAX as u64)
        && report_daemon(report).map_or(false, |id| !id.is_empty())
        && report["capture_source"] == "checkpoint"
        && report["capture"]["snapshot"] == report["snapshot"]
        && report["capture"]["daemon_session"] == report["daemon_session"]
        && report["capture"]["process_group"] == report["process_group"]
        && report["capture"]["inputs"].is_array()
        && report["capture"]["metadata_accesses"].is_array()
        && report["capture"]["errors"].is_array()
}

fn recover(mount: &OsStr, name: &str, dir: &Path) -> Result<(), String> {
    use crate::service::Command;
    let mut report = load_report(dir)?;
    if report["experiment"] != name || report["capture_complete"] == true {
        return Err("报告名称不一致或已经完成".into());
    }
    let state = crate::status(mount)?;
    let current_daemon = state["audit"]["session_id"]
        .as_str()
        .ok_or("缺少 daemon 会话状态")?;
    let same_daemon = report_daemon(&report).map_or(true, |id| id == current_daemon);
    let session = state["experiments"]
        .as_array()
        .ok_or("缺少会话状态")?
        .iter()
        .find(|s| s["name"] == name && s["pgid"] == report["process_group"]);
    if let Some(session) = session.filter(|_| same_daemon) {
        let pgid = session["pgid"]
            .as_u64()
            .filter(|p| *p > 0 && *p <= i32::MAX as u64)
            .ok_or("无效进程组")? as i32;
        let alive = unsafe { libc::kill(-pgid, 0) };
        if alive == 0 || std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH) {
            return Err("实验进程组仍在运行或无法确认已结束，不能结束追踪".into());
        }
        report["capture"] = request(
            mount,
            Command::ExperimentFinish {
                name: name.into(),
                daemon_session: report_daemon(&report),
            },
        )?;
        report["capture_source"] = json!("live_recovery");
        mark_incomplete(
            &mut report,
            "runner_interrupted",
            "运行器未完成报告收尾；仅保留实际观测记录，不补造未知退出状态",
        );
    } else {
        // A restarted daemon cannot reconstruct missing observations. Do not
        // finish a new same-named experiment, or invent counters/exit status.
        if !valid_checkpoint(&report) {
            return Err("运行状态已丢失且没有有效的已保存采集摘要；仅可尝试恢复已提交输入".into());
        }
        mark_incomplete(
            &mut report,
            "daemon_interrupted",
            "原 daemon 会话不可用；仅保留最后已保存摘要，此后访问未知",
        );
    }
    let snapshot_present = state["snapshots"].as_array().map_or(false, |rows| {
        rows.iter().any(|s| s["name"] == report["snapshot"])
    });
    report["input_snapshot_name_present"] = json!(snapshot_present);
    // A name can be deleted and reused. Never certify an old input merely
    // because a same-named snapshot exists in this or an imported store.
    report["input_version_available"] = Value::Null;
    report["input_version_status"] = json!("unverified");
    if !snapshot_present {
        report["input_version_available"] = json!(false);
        report["input_version_status"] = json!("missing");
    } else if let Some(expected) = report["capture"]["input_snapshot_sha256"].as_str() {
        match request(mount, Command::ExperimentInputVersion { name: name.into() }) {
            Ok(current) => {
                let matches = current["sha256"] == expected;
                report["input_version_available"] = json!(matches);
                report["input_version_status"] = json!(if matches {
                    "matched"
                } else {
                    "different_version"
                });
            }
            Err(error) => report["input_version_error"] = json!(error),
        }
    }
    save_report(dir, &report)?;
    println!("已保存中断实验的观测记录；报告不完整，未知退出状态和缺失读取不会补造。");
    Ok(())
}
static CANCELLED: AtomicBool = AtomicBool::new(false);
extern "C" fn cancel(_: i32) {
    CANCELLED.store(true, Ordering::Relaxed);
}

/// Internal exec gate: the command cannot touch its input until capture is registered.
pub fn child_exec(args: &[OsString]) -> Result<(), String> {
    if args.len() < 2 {
        return Err("invalid experiment exec gate".into());
    }
    let fd: i32 = args[0]
        .to_str()
        .ok_or("invalid gate fd")?
        .parse()
        .map_err(|_| "invalid gate fd")?;
    let mut gate = unsafe { File::from_raw_fd(fd) };
    let mut byte = [0];
    gate.read_exact(&mut byte).map_err(|e| e.to_string())?;
    drop(gate);
    std::env::set_current_dir(std::env::var_os("TEAMFS_INPUT").ok_or("missing input directory")?)
        .map_err(|e| e.to_string())?;
    Err(Process::new(&args[1]).args(&args[2..]).exec().to_string())
}

pub fn cli(args: &[OsString]) -> Result<(), String> {
    use crate::service::Command;
    match args.first().and_then(|s|s.to_str()) {
        Some("run") if args.len() >= 7 && args[3]=="--output" && args[5]=="--" => run(&args[1], &args[2], Path::new(&args[4]), &args[6..],None),
        Some("replay") if args.len()==6 && args[4]=="--output" => {
            let report=load_report(Path::new(&args[2]))?;
            if report["cancelled"]==true || report["signal"].is_number() {return Err("中断命令没有正常结束，不能自动作为完成实验重放".into());}
            verify(&args[1],Path::new(&args[2]))?;
            let argv:Vec<Vec<u8>>=serde_json::from_value(report["command_bytes"].clone()).map_err(|_|"缺少原命令参数")?;
            let command:Vec<OsString>=argv.into_iter().map(OsString::from_vec).collect();
            if command.is_empty(){return Err("原命令为空".into());}
            run(&args[1],&args[3],Path::new(&args[5]),&command,Some(report["snapshot"].as_str().unwrap().into()))
        },
        Some("show") if args.len()==2 => {
            let report = load_report(Path::new(&args[1]))?;
            println!("{}",serde_json::to_string_pretty(&report).unwrap()); Ok(())
        },
        Some("verify") if args.len()==3 => verify(&args[1], Path::new(&args[2])),
        Some("recover") if args.len()==4 => {
            let name=args[2].to_str().ok_or("实验名称必须是 UTF-8")?;
            recover(&args[1], name, Path::new(&args[3]))
        },
        Some("restore") if args.len()==4 => {
            let snapshot = snapshot_name(args[2].to_str().ok_or("实验名称必须是 UTF-8")?).map_err(|e|format!("无效名称：{e}"))?;
            request(&args[1], Command::RestoreTree { snapshot, source:b".".to_vec(), destination:args[3].as_bytes().to_vec(), dry_run:false })?;
            println!("实验输入版本已恢复到新目录；等待后续同步。"); Ok(())
        },
        Some("delete") if args.len()==3 => {
            request(&args[1],Command::ExperimentDelete {name:args[2].to_str().ok_or("实验名称必须是 UTF-8")?.into()})?;
            println!("已删除实验输入快照；外部报告仍保留，但不能再从此存储取回该版本。"); Ok(())
        },
        _ => Err("用法：experiment run <挂载点> <名称> --output <新报告目录> -- <命令> [参数...]；或 show/verify/restore/delete".into())
    }
}
fn run(
    mount: &OsStr,
    name: &OsStr,
    output: &Path,
    command: &[OsString],
    source_snapshot: Option<String>,
) -> Result<(), String> {
    use crate::service::Command;
    let name = name.to_str().ok_or("实验名称必须是 UTF-8")?.to_owned();
    let snapshot = snapshot_name(&name).map_err(|e| format!("无效实验名称：errno={e}"))?;
    let state = crate::status(mount)?;
    let mount = Path::new(mount).canonicalize().map_err(|e| e.to_string())?;
    let output = crate::backup::target_path(output)?;
    if output.starts_with(&mount) {
        return Err("报告和输出必须位于挂载点之外".into());
    }
    if let Ok(bytes) = serde_json::from_value::<Vec<u8>>(state["store_path_bytes"].clone()) {
        if output.starts_with(PathBuf::from(OsString::from_vec(bytes)).parent().unwrap()) {
            return Err("报告目录必须位于后端存储目录之外".into());
        }
    }
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&output)
        .map_err(|e| e.to_string())?;
    std::fs::create_dir(output.join("outputs")).map_err(|e| e.to_string())?;
    let input = mount.join(".teamfs/snapshots").join(&snapshot);
    let mut report = json!({"schema_version":1,"teamfs":env!("CARGO_PKG_VERSION"),"experiment":name,"snapshot":snapshot,
        "mode":state["mode"],"phase":"prepared","outcome":"prepared","capture_complete":false,"started_unix":time::get_time().sec,
        "daemon_session":state["audit"]["session_id"],"checkpoint_interval_seconds":CHECKPOINT_INTERVAL.as_secs(),
        "checkpoint_sequence":0,"input_version_persistent":state["mode"]=="persistent",
        "command_bytes":command.iter().map(|s|s.as_bytes()).collect::<Vec<_>>(),
        "command_display":command.iter().map(|s|crate::history::display_path(s.as_bytes())).collect::<Vec<_>>(),
        "input_directory_bytes":input.as_os_str().as_bytes(),"output_directory_bytes":output.as_os_str().as_bytes(),
        "scope":"Pinned TeamFS snapshot and this process group only. External files, network inputs, software environment and processes leaving the group are not tracked. FUSE read bytes do not prove algorithmic use; metadata-only and opened-only accesses are separate. Requests the kernel rejects or serves from its caches never reach FUSE. Snapshot files use direct I/O: shared mmap is unsupported, private read-only mappings are read through the page cache at readahead granularity."});
    save_report(&output, &report)?;
    if state["mode"] == "memory" {
        eprintln!("memory 模式：摘要可保留在外部报告中，但输入版本不会跨 daemon 重启保留。");
    }
    let mut fds = [0; 2];
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    let gate_read = unsafe { File::from_raw_fd(fds[0]) };
    let mut gate_write = unsafe { File::from_raw_fd(fds[1]) };
    let gate_fd = gate_read.as_raw_fd();
    let runner_pid = std::process::id();
    let mut process = Process::new(std::env::current_exe().map_err(|e| e.to_string())?);
    process
        .arg("__experiment-exec")
        .arg("198")
        .args(command)
        .env("TEAMFS_INPUT", &input)
        .env("TEAMFS_OUTPUT", output.join("outputs"))
        .stdout(Stdio::from(
            OpenOptions::new()
                .create_new(true)
                .write(true)
                .mode(0o600)
                .open(output.join("stdout.txt"))
                .map_err(|e| e.to_string())?,
        ))
        .stderr(Stdio::from(
            OpenOptions::new()
                .create_new(true)
                .write(true)
                .mode(0o600)
                .open(output.join("stderr.txt"))
                .map_err(|e| e.to_string())?,
        ));
    unsafe {
        process.pre_exec(move || {
            if libc::setpgid(0, 0) != 0
                || libc::dup2(gate_fd, 198) < 0
                || libc::fcntl(198, libc::F_SETFD, 0) < 0
                || libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0
                || libc::getppid() != runner_pid as i32
            {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = process.spawn().map_err(|e| e.to_string())?;
    drop(gate_read);
    let pgid = child.id();
    let mut active = false;
    CANCELLED.store(false, Ordering::Relaxed);
    unsafe {
        libc::signal(libc::SIGINT, cancel as *const () as libc::sighandler_t);
        libc::signal(libc::SIGTERM, cancel as *const () as libc::sighandler_t);
    }
    let outcome = (|| -> Result<std::process::ExitStatus, String> {
        request(
            mount.as_os_str(),
            Command::ExperimentStart {
                name: name.clone(),
                pgid,
                source_snapshot: source_snapshot.clone(),
            },
        )?;
        active = true;
        report["phase"] = json!("running");
        report["outcome"] = json!("running");
        report["process_group"] = json!(pgid);
        report["replay_source_snapshot"] = json!(source_snapshot);
        checkpoint(mount.as_os_str(), &name, &output, &mut report)?;
        // Cancellation can arrive while the initial store commit is waiting.
        // Keep that child behind the gate until the loop kills and reaps it.
        if !CANCELLED.load(Ordering::Relaxed) {
            gate_write.write_all(&[1]).map_err(|e| e.to_string())?;
        }
        let mut last_checkpoint = Instant::now();
        let status = loop {
            if CANCELLED.load(Ordering::Relaxed) {
                unsafe {
                    libc::kill(-(pgid as i32), libc::SIGKILL);
                }
            }
            if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
                break status;
            }
            if last_checkpoint.elapsed() >= CHECKPOINT_INTERVAL {
                checkpoint(mount.as_os_str(), &name, &output, &mut report)?;
                last_checkpoint = Instant::now();
            }
            std::thread::sleep(std::time::Duration::from_millis(30));
        };
        // Ordinary subprocesses are in the group; do not let background readers outlive capture.
        report["finished_unix"] = json!(time::get_time().sec);
        report["exit_code"] = json!(status.code());
        report["signal"] = json!(status.signal());
        report["cancelled"] = json!(CANCELLED.load(Ordering::Relaxed));
        unsafe {
            libc::kill(-(pgid as i32), libc::SIGKILL);
        }
        let capture = request(
            mount.as_os_str(),
            Command::ExperimentFinish {
                name: name.clone(),
                daemon_session: report_daemon(&report),
            },
        )?;
        active = false;
        report["capture"] = capture;
        report["capture_complete"] = json!(true);
        report["capture_source"] = json!("final");
        report["phase"] = json!("finished");
        report["finished_unix"] = json!(time::get_time().sec);
        report["exit_code"] = json!(status.code());
        report["signal"] = json!(status.signal());
        report["cancelled"] = json!(CANCELLED.load(Ordering::Relaxed));
        report["outcome"] = json!(if CANCELLED.load(Ordering::Relaxed) {
            "cancelled"
        } else if status.success() {
            "succeeded"
        } else {
            "program_failed"
        });
        save_report(&output, &report)?;
        Ok(status)
    })();
    if let Err(error) = &outcome {
        unsafe {
            libc::kill(-(pgid as i32), libc::SIGKILL);
        }
        let _ = child.wait();
        if active {
            if let Ok(capture) = request(
                mount.as_os_str(),
                Command::ExperimentFinish {
                    name,
                    daemon_session: report_daemon(&report),
                },
            ) {
                report["capture"] = capture;
                report["capture_source"] = json!("failure_cleanup");
            }
        }
        let failure = if report["phase"] == "prepared" {
            "start_failed"
        } else if active && report["capture_source"] == "checkpoint" {
            "daemon_unavailable"
        } else {
            "capture_failed"
        };
        mark_incomplete(&mut report, failure, error);
        report["phase"] = json!("failed");
        if let Err(save_error) = save_report(&output, &report) {
            eprintln!("未能保存失败状态（最后成功提交的摘要仍可用于恢复）：{save_error}");
        }
    }
    let status = outcome?;
    println!(
        "实验结束：{}\n报告：{}\n输入目录：{}",
        report["experiment"],
        output.join("manifest.json").display(),
        input.display()
    );
    for (_, warning) in capture_warnings(&report["capture"]) {
        eprintln!("警告：{warning}");
    }
    if !status.success() {
        std::process::exit(status.code().unwrap_or(128 + status.signal().unwrap_or(1)));
    }
    Ok(())
}
fn load_report(dir: &Path) -> Result<Value, String> {
    let value: Value = serde_json::from_slice(
        &std::fs::read(dir.join("manifest.json")).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    if value["schema_version"] != 1 || value["experiment"].as_str().is_none() {
        return Err("不是支持的实验报告".into());
    }
    let snapshot =
        snapshot_name(value["experiment"].as_str().unwrap()).map_err(|_| "无效实验名称")?;
    if value["snapshot"] != snapshot {
        return Err("报告快照名称不一致".into());
    }
    Ok(value)
}
fn verify(mount: &OsStr, dir: &Path) -> Result<(), String> {
    crate::status(mount)?;
    let report = load_report(dir)?;
    if report["capture_complete"] != true {
        return Err("报告未完成，不能作为完整输入清单验证".into());
    }
    if report["capture"]["unpinned_mount_accesses"]
        .as_u64()
        .unwrap_or(0)
        > 0
        || report["capture"]["external_link_accesses"]
            .as_u64()
            .unwrap_or(0)
            > 0
    {
        let reasons: Vec<String> = capture_warnings(&report["capture"])
            .into_iter()
            .filter_map(|(blocking, warning)| blocking.then_some(warning))
            .collect();
        return Err(format!(
            "{}；不能作为全部挂载内输入已固定的实验验证或重放",
            reasons.join("；")
        ));
    }
    let name = report["experiment"].as_str().unwrap();
    let snapshot = snapshot_name(name).map_err(|_| "无效实验名称")?;
    if report["snapshot"] != snapshot {
        return Err("报告快照名称不一致".into());
    }
    let base = Path::new(mount).join(".teamfs/snapshots").join(snapshot);
    if !base.is_dir() {
        return Err("实验输入快照不存在；报告自身不包含文件正文".into());
    }
    if let Some(expected) = report["capture"]["input_snapshot_sha256"].as_str() {
        let current = request(
            mount,
            crate::service::Command::ExperimentInputVersion { name: name.into() },
        )?;
        if current["sha256"] != expected {
            return Err("同名快照的内容或元数据已改变，不是报告对应的原输入版本".into());
        }
    }
    let inputs = report["capture"]["inputs"]
        .as_array()
        .ok_or("缺少输入清单")?;
    for entry in inputs {
        let path: Vec<u8> =
            serde_json::from_value(entry["path_bytes"].clone()).map_err(|_| "无效路径")?;
        let mut file = base.clone();
        for part in path.split(|b| *b == b'/') {
            MemFs::valid_name(OsStr::from_bytes(part)).map_err(|_| "无效输入相对路径")?;
            file.push(OsStr::from_bytes(part));
        }
        let mut f = File::open(file).map_err(|e| e.to_string())?;
        let mut hash = Sha256::new();
        let mut buffer = vec![0; crate::content::PAGE];
        loop {
            let n = f.read(&mut buffer).map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            hash.update(&buffer[..n]);
        }
        if entry["sha256"] != format!("{:x}", hash.finalize()) {
            return Err(format!("输入校验不一致：{}", entry["path_display"]));
        }
    }
    println!("验证通过：{} 个文件与实验输入版本一致。", inputs.len());
    for (_, notice) in capture_warnings(&report["capture"]) {
        println!("注意：{notice}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn input_version_digest_covers_unread_content_and_metadata_in_stable_order() {
        let fs = MemFs::new(1000, 1000);
        let tree = fs.export();
        let expected = input_tree_digest(&tree).unwrap();
        let mut reordered = tree.clone();
        reordered.nodes.reverse();
        assert_eq!(input_tree_digest(&reordered).unwrap(), expected);
        let mut content_changed = tree.clone();
        let file = content_changed
            .nodes
            .iter_mut()
            .find(|n| n.attr.kind == Kind::File)
            .unwrap();
        file.data = b"replacement content".to_vec().into();
        assert_ne!(input_tree_digest(&content_changed).unwrap(), expected);
        let mut metadata_changed = tree.clone();
        metadata_changed.nodes[0].attr.mode ^= 0o100;
        assert_ne!(input_tree_digest(&metadata_changed).unwrap(), expected);
    }
    #[test]
    fn checkpoint_recovery_rejects_foreign_or_partial_evidence() {
        let mut report = json!({"checkpoint_sequence":2,"capture_source":"checkpoint",
            "daemon_session":"original","snapshot":"experiment.test","process_group":42,
            "capture":{"daemon_session":"original","snapshot":"experiment.test","process_group":42,
                "inputs":[],"metadata_accesses":[],"errors":[]}});
        assert!(valid_checkpoint(&report));
        report["capture"]["daemon_session"] = json!("new-daemon");
        assert!(!valid_checkpoint(&report));
        report["capture"]["daemon_session"] = json!("original");
        report["capture"]["inputs"] = Value::Null;
        assert!(!valid_checkpoint(&report));
        assert!(!valid_checkpoint(&json!({"checkpoint_sequence":1})));
    }
    #[test]
    fn incomplete_recovery_never_invents_unknown_termination_or_counts() {
        let capture = json!({"inputs":[{"read_calls":4,"read_bytes":17}]});
        let mut report =
            json!({"capture":capture,"capture_source":"checkpoint","checkpoint_sequence":2});
        mark_incomplete(&mut report, "daemon_interrupted", "daemon lost");
        assert_eq!(report["capture"], capture);
        assert_eq!(report["capture_complete"], false);
        assert_eq!(report["unknown_after_checkpoint"], true);
        assert!(report.get("exit_code").is_none());
        assert!(report.get("signal").is_none());
        report["exit_code"] = json!(7); // A runner-observed outcome must not be erased.
        mark_incomplete(&mut report, "capture_failed", "export failed");
        assert_eq!(report["exit_code"], 7);
    }
    #[test]
    fn atomic_report_failure_keeps_previous_saved_summary() {
        let dir = std::env::temp_dir().join(format!(
            "teamfs-report-{}-{}",
            std::process::id(),
            time::get_time().nsec
        ));
        std::fs::create_dir(&dir).unwrap();
        let before = json!({"schema_version":1,"experiment":"checkpoint","snapshot":"experiment.checkpoint","checkpoint_sequence":1});
        save_report(&dir, &before).unwrap();
        // A missing parent fails before publication; the existing manifest remains valid.
        assert!(save_report(&dir.join("missing"), &json!({"checkpoint_sequence":2})).is_err());
        assert_eq!(load_report(&dir).unwrap(), before);
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn link_resolution_checks_real_parent_paths_and_intermediate_links() {
        let mut fs = MemFs::new(1000, 1000);
        fs.create(ROOT, OsStr::new("source"), Kind::File, 0o600, 1000, 1000)
            .unwrap();
        let nested = fs
            .create(
                ROOT,
                OsStr::new("nested"),
                Kind::Directory,
                0o700,
                1000,
                1000,
            )
            .unwrap();
        let safe = fs
            .symlink(nested, OsStr::new("safe"), b"../source", 1000, 1000)
            .unwrap();
        let escape = fs
            .symlink(nested, OsStr::new("escape"), b"../../host", 1000, 1000)
            .unwrap();
        fs.symlink(nested, OsStr::new("up"), b"..", 1000, 1000)
            .unwrap();
        let chain = fs
            .symlink(ROOT, OsStr::new("chain"), b"nested/up/../host", 1000, 1000)
            .unwrap();
        let a = fs.symlink(ROOT, OsStr::new("a"), b"b", 1000, 1000).unwrap();
        fs.symlink(ROOT, OsStr::new("b"), b"a", 1000, 1000).unwrap();
        let tree = fs.export();
        let check = |ino| {
            link_may_escape(
                &tree,
                tree.nodes.iter().position(|n| n.attr.ino == ino).unwrap(),
            )
            .unwrap()
        };
        assert!(!check(safe));
        assert!(check(escape));
        assert!(check(chain));
        assert!(check(a));
    }
    #[test]
    fn thread_ids_resolve_to_their_process() {
        let (tid_tx, tid_rx) = std::sync::mpsc::channel();
        let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
        let worker = std::thread::spawn(move || {
            tid_tx
                .send(unsafe { libc::syscall(libc::SYS_gettid) } as u32)
                .unwrap();
            let _ = done_rx.recv();
        });
        let tid = tid_rx.recv().unwrap();
        assert_ne!(tid, std::process::id());
        assert_eq!(process_id(tid), std::process::id());
        assert_eq!(process_id(std::process::id()), std::process::id());
        let mut session = Session::new("experiment.unit".into(), 1);
        assert_eq!(session.process(tid), std::process::id());
        done_tx.send(()).unwrap();
        worker.join().unwrap();
        // The cached answer survives the thread; an unknown task maps to itself.
        assert_eq!(session.process(tid), std::process::id());
        assert_eq!(process_id(u32::MAX), u32::MAX);
    }
    #[test]
    fn warnings_separate_blocking_reasons_from_notices() {
        let capture = json!({"unpinned_mount_accesses":2,"unpinned_mount_files":[{"location":"business","path_display":"数据.csv"}],
            "external_link_accesses":0,"other_group_snapshot_accesses":3,"other_group_pids":[42]});
        let warnings = capture_warnings(&capture);
        assert_eq!(warnings.len(), 2);
        assert!(warnings[0].0 && warnings[0].1.contains("business:数据.csv"));
        assert!(!warnings[1].0 && warnings[1].1.contains("[42]"));
        assert!(capture_warnings(&json!({})).is_empty());
    }
}
