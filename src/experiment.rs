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
const OUTPUT_SCOPE: &str = "报告目录内 outputs/ 下的全部普通文件，以及顶层 stdout.txt、stderr.txt。软链接和特殊文件不纳入；这是报告收尾时的摘要，不证明程序没有写其他挂载点外文件。";
const ENVIRONMENT_DISCLAIMER: &str = "轻量环境摘要，不是完整环境归档；不包含已安装软件包、库版本、全部环境变量、硬件细节或外部文件。";

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

fn trimmed_file(path: &str) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|text| text.trim().to_owned())
        .filter(|text| !text.is_empty())
}
fn os_release() -> Option<String> {
    std::fs::read_to_string("/etc/os-release")
        .ok()
        .and_then(|text| {
            text.lines().find_map(|line| {
                line.strip_prefix("PRETTY_NAME=")
                    .map(|value| value.trim_matches('"').to_owned())
            })
        })
        .filter(|value| !value.is_empty())
}
fn uname_values() -> (Option<String>, Option<String>, Option<String>) {
    let mut uts: libc::utsname = unsafe { std::mem::zeroed() };
    if unsafe { libc::uname(&mut uts) } != 0 {
        return (None, None, None);
    }
    let field = |bytes: &[libc::c_char]| {
        let raw: Vec<u8> = bytes
            .iter()
            .take_while(|byte| **byte != 0)
            .map(|byte| *byte as u8)
            .collect();
        let value = String::from_utf8_lossy(&raw).into_owned();
        (!value.is_empty()).then_some(value)
    };
    (
        field(&uts.sysname),
        field(&uts.release),
        field(&uts.machine),
    )
}
fn python_version() -> Option<String> {
    let output = Process::new("python3").arg("--version").output().ok()?;
    let text = if output.stdout.is_empty() {
        &output.stderr
    } else {
        &output.stdout
    };
    let text = String::from_utf8_lossy(text).trim().to_owned();
    (!text.is_empty()).then_some(text)
}
fn mem_total_kib() -> Option<u64> {
    std::fs::read_to_string("/proc/meminfo")
        .ok()
        .and_then(|text| {
            text.lines().find_map(|line| {
                line.strip_prefix("MemTotal:")?
                    .split_whitespace()
                    .next()?
                    .parse()
                    .ok()
            })
        })
}
/// A deliberately small summary for interpreting a result, never an environment archive.
fn environment_summary() -> Value {
    let (sysname, uname_release, machine) = uname_values();
    let kernel = trimmed_file("/proc/sys/kernel/osrelease").or(uname_release);
    let hostname = trimmed_file("/proc/sys/kernel/hostname");
    let cpus = unsafe { libc::sysconf(libc::_SC_NPROCESSORS_ONLN) };
    json!({
        "os_release": os_release(),
        "kernel": kernel,
        "sysname": sysname,
        "machine": machine,
        "hostname": hostname,
        "uid": unsafe { libc::geteuid() },
        "gid": unsafe { libc::getegid() },
        "python3": python_version(),
        "cpu_count": (cpus > 0).then_some(cpus as u64),
        "mem_total_kib": mem_total_kib(),
        "wsl": kernel.as_deref().map_or(false, |value| value.to_ascii_lowercase().contains("microsoft")),
        "teamfs_version": env!("CARGO_PKG_VERSION"),
        "captured_unix": time::get_time().sec,
        "disclaimer": ENVIRONMENT_DISCLAIMER,
    })
}
fn walk_outputs(root: &Path, relative: &Path, files: &mut Vec<PathBuf>) -> Result<(), String> {
    let dir = root.join(relative);
    let mut entries: Vec<_> = std::fs::read_dir(&dir)
        .map_err(|error| format!("读取输出目录 {} 失败：{error}", dir.display()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let name = entry.file_name();
        let child = relative.join(&name);
        let kind = entry.file_type().map_err(|error| error.to_string())?;
        if kind.is_dir() {
            walk_outputs(root, &child, files)?;
        } else if kind.is_file() {
            files.push(child);
        }
    }
    Ok(())
}
fn hash_regular_file(path: &Path) -> Result<(String, u64), String> {
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|error| format!("{}: {error}", path.display()))?;
    let mut hash = Sha256::new();
    let mut size = 0u64;
    let mut buffer = vec![0; crate::content::PAGE];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| format!("{}: {error}", path.display()))?;
        if read == 0 {
            break;
        }
        hash.update(&buffer[..read]);
        size += read as u64;
    }
    Ok((format!("{:x}", hash.finalize()), size))
}
fn output_digests(dir: &Path) -> Result<Value, String> {
    let outputs = dir.join("outputs");
    let mut relative = Vec::new();
    if outputs.is_dir() {
        walk_outputs(&outputs, Path::new(""), &mut relative)?;
    }
    let mut rows = Vec::new();
    let mut errors = Vec::new();
    for rel in relative {
        let mut display = b"outputs/".to_vec();
        display.extend_from_slice(rel.as_os_str().as_bytes());
        let path = outputs.join(&rel);
        match hash_regular_file(&path) {
            Ok((sha256, bytes)) => rows.push(json!({
                "path": crate::history::display_path(&display),
                "path_display": crate::history::display_path(&display),
                "path_bytes": display,
                "bytes": bytes,
                "sha256": sha256,
            })),
            Err(error) => {
                errors.push(format!("{}: {error}", crate::history::display_path(&display)));
                rows.push(json!({
                    "path": crate::history::display_path(&display),
                    "path_display": crate::history::display_path(&display),
                    "path_bytes": display,
                    "bytes": Value::Null,
                    "sha256": Value::Null,
                    "error": error,
                }));
            }
        }
    }
    for name in ["stdout.txt", "stderr.txt"] {
        let path = dir.join(name);
        if path.is_file() {
            match hash_regular_file(&path) {
                Ok((sha256, bytes)) => rows.push(json!({
                    "path": name, "path_display": name, "path_bytes": name.as_bytes(),
                    "bytes": bytes, "sha256": sha256,
                })),
                Err(error) => {
                    errors.push(format!("{name}: {error}"));
                    rows.push(json!({
                        "path": name, "path_display": name, "path_bytes": name.as_bytes(),
                        "bytes": Value::Null, "sha256": Value::Null, "error": error,
                    }));
                }
            }
        }
    }
    rows.sort_by(|left, right| {
        let left: Vec<u8> = serde_json::from_value(left["path_bytes"].clone()).unwrap_or_default();
        let right: Vec<u8> =
            serde_json::from_value(right["path_bytes"].clone()).unwrap_or_default();
        left.cmp(&right)
    });
    let total: u64 = rows
        .iter()
        .filter_map(|row| row["bytes"].as_u64())
        .sum();
    Ok(json!({
        "captured": true,
        "complete": errors.is_empty(),
        "files": rows,
        "count": rows.len(),
        "total_bytes": total,
        "errors": errors,
        "captured_unix": time::get_time().sec,
        "scope": OUTPUT_SCOPE,
    }))
}
fn output_capture_error(error: &str) -> Value {
    json!({
        "captured": false,
        "complete": false,
        "files": [],
        "count": 0,
        "total_bytes": 0,
        "errors": [error],
        "captured_unix": time::get_time().sec,
        "scope": OUTPUT_SCOPE,
    })
}
fn capture_outputs(report: &mut Value, output: &Path) {
    report["outputs"] = match output_digests(output) {
        Ok(value) => value,
        Err(error) => output_capture_error(&error),
    };
}
fn safe_relative_path(bytes: &[u8]) -> Result<PathBuf, String> {
    if bytes.is_empty() || bytes.starts_with(b"/") {
        return Err("输出路径必须是报告目录内的相对路径".into());
    }
    let mut path = PathBuf::new();
    for part in bytes.split(|byte| *byte == b'/') {
        if part.is_empty() || part == b"." || part == b".." {
            return Err("输出路径包含空段、. 或 ..".into());
        }
        path.push(OsStr::from_bytes(part));
    }
    Ok(path)
}
fn report_path(row: &Value) -> Result<Vec<u8>, String> {
    serde_json::from_value(row["path_bytes"].clone()).map_err(|_| "报告包含无效的 path_bytes".into())
}
fn row_map(rows: &[Value], label: &str) -> Result<BTreeMap<Vec<u8>, Value>, String> {
    let mut map = BTreeMap::new();
    for row in rows {
        let path = report_path(row)?;
        if map.insert(path.clone(), row.clone()).is_some() {
            return Err(format!(
                "{label}中存在重复路径：{}",
                crate::history::display_path(&path)
            ));
        }
    }
    Ok(map)
}
fn row_display(row: &Value) -> Option<&str> {
    row["path_display"]
        .as_str()
        .or_else(|| row["path"].as_str())
}
fn display_for(path: &[u8], left: Option<&Value>, right: Option<&Value>) -> String {
    right
        .and_then(row_display)
        .or_else(|| left.and_then(row_display))
        .map(str::to_owned)
        .unwrap_or_else(|| crate::history::display_path(path))
}
fn union_paths(
    left: &BTreeMap<Vec<u8>, Value>,
    right: &BTreeMap<Vec<u8>, Value>,
) -> Vec<Vec<u8>> {
    let mut paths: Vec<Vec<u8>> = left.keys().chain(right.keys()).cloned().collect();
    paths.sort();
    paths.dedup();
    paths
}
/// Compare file content only. Operational metadata (names, PIDs, timestamps,
/// counters) is reported separately and never counted as a content change.
fn compare_reports(left: &Value, right: &Value) -> Result<Value, String> {
    let empty = Vec::new();
    let left_inputs = row_map(
        left["capture"]["inputs"].as_array().unwrap_or(&empty),
        "报告 A 输入",
    )?;
    let right_inputs = row_map(
        right["capture"]["inputs"].as_array().unwrap_or(&empty),
        "报告 B 输入",
    )?;
    let mut inputs_added = Vec::new();
    let mut inputs_removed = Vec::new();
    let mut inputs_changed = Vec::new();
    let mut inputs_access = Vec::new();
    let mut inputs_unchanged = Vec::new();
    for path in union_paths(&left_inputs, &right_inputs) {
        match (left_inputs.get(&path), right_inputs.get(&path)) {
            (Some(before), None) => inputs_removed.push(json!({
                "path_bytes": path,
                "path_display": display_for(&path, Some(before), None),
                "sha256": before["sha256"],
                "access_class": before["access_class"],
            })),
            (None, Some(after)) => inputs_added.push(json!({
                "path_bytes": path,
                "path_display": display_for(&path, None, Some(after)),
                "sha256": after["sha256"],
                "access_class": after["access_class"],
            })),
            (Some(before), Some(after)) => {
                if before["sha256"] != after["sha256"] {
                    inputs_changed.push(json!({
                        "path_bytes": path,
                        "path_display": display_for(&path, Some(before), Some(after)),
                        "a_sha256": before["sha256"],
                        "b_sha256": after["sha256"],
                    }));
                } else if before["access_class"] != after["access_class"] {
                    inputs_access.push(json!({
                        "path_bytes": path,
                        "path_display": display_for(&path, Some(before), Some(after)),
                        "a_access_class": before["access_class"],
                        "b_access_class": after["access_class"],
                    }));
                } else {
                    inputs_unchanged.push(json!({
                        "path_bytes": path,
                        "path_display": display_for(&path, Some(before), Some(after)),
                    }));
                }
            }
            (None, None) => unreachable!(),
        }
    }

    let left_outputs = left["outputs"].as_object();
    let right_outputs = right["outputs"].as_object();
    let left_captured = left_outputs
        .and_then(|outputs| outputs.get("captured"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let right_captured = right_outputs
        .and_then(|outputs| outputs.get("captured"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let outputs_available = left_captured && right_captured;
    let mut outputs_added = Vec::new();
    let mut outputs_removed = Vec::new();
    let mut outputs_changed = Vec::new();
    let mut outputs_unchanged = Vec::new();
    if outputs_available {
        let read_rows = |report: &Value| -> Vec<Value> {
            report["outputs"]["files"]
                .as_array()
                .cloned()
                .unwrap_or_default()
        };
        let before_outputs = row_map(&read_rows(left), "报告 A 输出")?;
        let after_outputs = row_map(&read_rows(right), "报告 B 输出")?;
        for path in union_paths(&before_outputs, &after_outputs) {
            match (before_outputs.get(&path), after_outputs.get(&path)) {
                (Some(before), None) => outputs_removed.push(json!({
                    "path_bytes": path,
                    "path_display": display_for(&path, Some(before), None),
                    "sha256": before["sha256"],
                    "bytes": before["bytes"],
                })),
                (None, Some(after)) => outputs_added.push(json!({
                    "path_bytes": path,
                    "path_display": display_for(&path, None, Some(after)),
                    "sha256": after["sha256"],
                    "bytes": after["bytes"],
                })),
                (Some(before), Some(after)) => {
                    if before["sha256"] != after["sha256"] || before["bytes"] != after["bytes"] {
                        outputs_changed.push(json!({
                            "path_bytes": path,
                            "path_display": display_for(&path, Some(before), Some(after)),
                            "a_sha256": before["sha256"],
                            "b_sha256": after["sha256"],
                            "a_bytes": before["bytes"],
                            "b_bytes": after["bytes"],
                        }));
                    } else {
                        outputs_unchanged.push(json!({
                            "path_bytes": path,
                            "path_display": display_for(&path, Some(before), Some(after)),
                            "sha256": after["sha256"],
                            "bytes": after["bytes"],
                        }));
                    }
                }
                (None, None) => unreachable!(),
            }
        }
    }

    let exit_changed = left["exit_code"] != right["exit_code"]
        || left["signal"] != right["signal"]
        || left["outcome"] != right["outcome"];
    let run_context = json!({
        "experiment": {"a": left["experiment"], "b": right["experiment"], "changed": left["experiment"] != right["experiment"]},
        "started_unix": {"a": left["started_unix"], "b": right["started_unix"], "changed": left["started_unix"] != right["started_unix"]},
        "finished_unix": {"a": left["finished_unix"], "b": right["finished_unix"], "changed": left["finished_unix"] != right["finished_unix"]},
        "process_group": {"a": left["process_group"], "b": right["process_group"], "changed": left["process_group"] != right["process_group"]},
        "exit": {
            "a": {"exit_code": left["exit_code"], "signal": left["signal"], "outcome": left["outcome"]},
            "b": {"exit_code": right["exit_code"], "signal": right["signal"], "outcome": right["outcome"]},
            "changed": exit_changed,
        },
        "command": {"a": left["command_display"], "b": right["command_display"], "changed": left["command_display"] != right["command_display"]},
        "note": "实验名称、进程组/PID、开始与结束时间、采集序号、读取计数和退出状态属于运行元数据，不参与文件内容比较。",
    });
    let complete = left["capture_complete"] == true && right["capture_complete"] == true;
    let inputs_content_identical = inputs_added.is_empty()
        && inputs_removed.is_empty()
        && inputs_changed.is_empty();
    let outputs_identical = outputs_available
        && outputs_added.is_empty()
        && outputs_removed.is_empty()
        && outputs_changed.is_empty();
    let value = json!({
        "schema_version": 1,
        "compared_unix": time::get_time().sec,
        "report_a": {
            "experiment": left["experiment"], "snapshot": left["snapshot"],
            "capture_complete": left["capture_complete"], "outcome": left["outcome"],
            "exit_code": left["exit_code"], "signal": left["signal"],
        },
        "report_b": {
            "experiment": right["experiment"], "snapshot": right["snapshot"],
            "capture_complete": right["capture_complete"], "outcome": right["outcome"],
            "exit_code": right["exit_code"], "signal": right["signal"],
        },
        "inputs": {
            "summary": {
                "added": inputs_added.len(), "removed": inputs_removed.len(),
                "content_changed": inputs_changed.len(),
                "access_class_changed": inputs_access.len(),
                "unchanged": inputs_unchanged.len(),
            },
            "added": inputs_added, "removed": inputs_removed,
            "content_changed": inputs_changed,
            "access_class_changed": inputs_access,
            "unchanged": inputs_unchanged,
        },
        "outputs": {
            "available": outputs_available,
            "complete": left["outputs"]["complete"].as_bool().unwrap_or(false)
                && right["outputs"]["complete"].as_bool().unwrap_or(false),
            "summary": {
                "added": outputs_added.len(), "removed": outputs_removed.len(),
                "changed": outputs_changed.len(), "unchanged": outputs_unchanged.len(),
            },
            "added": outputs_added, "removed": outputs_removed,
            "changed": outputs_changed, "unchanged": outputs_unchanged,
            "note": if outputs_available {
                "只比较两个报告都采集到的输出文件摘要。"
            } else {
                "至少一个报告没有采集输出摘要（旧版报告或采集失败），不能据此判断输出新增或删除。"
            },
        },
        "run_context": run_context,
        "volatile_fields_ignored": [
            "experiment", "snapshot", "process_group", "capture.inputs[].pids",
            "started_unix", "finished_unix", "checkpoint_sequence", "daemon_session",
            "input_directory_bytes", "output_directory_bytes",
            "capture.inputs[].read_calls", "capture.inputs[].read_bytes",
            "exit_code", "signal", "outcome"
        ],
        "verdict": {
            "both_capture_complete": complete,
            "inputs_content_identical": inputs_content_identical,
            "inputs_access_identical": inputs_access.is_empty(),
            "outputs_identical": outputs_identical,
            "note": "inputs_content_identical 只比较路径与 SHA256；访问分类变化单独列出。outputs_identical 仅在两个报告都采集了输出摘要时成立。",
        },
        "scope": "比较只读取两份外部报告中的输入清单、输出摘要和运行元数据，不读取输入正文，也不执行实验。",
    });
    Ok(value)
}
fn print_compare(value: &Value) {
    println!(
        "报告 A：{}（实验 {}）",
        value["report_a"]["directory"].as_str().unwrap_or("?"),
        value["report_a"]["experiment"].as_str().unwrap_or("?")
    );
    println!(
        "报告 B：{}（实验 {}）",
        value["report_b"]["directory"].as_str().unwrap_or("?"),
        value["report_b"]["experiment"].as_str().unwrap_or("?")
    );
    for (_title, section) in [("输入", "inputs"), ("输出", "outputs")] {
        if section == "outputs" && value["outputs"]["available"] != true {
            println!("输出：{}", value["outputs"]["note"].as_str().unwrap_or("不可比较"));
            continue;
        }
        let summary = &value[section]["summary"];
        if section == "inputs" {
            println!(
                "输入：新增 {}，删除 {}，内容变化 {}，访问分类变化 {}，未变 {}",
                summary["added"], summary["removed"], summary["content_changed"],
                summary["access_class_changed"], summary["unchanged"]
            );
        } else {
            println!(
                "输出：新增 {}，删除 {}，内容变化 {}，未变 {}",
                summary["added"], summary["removed"], summary["changed"], summary["unchanged"]
            );
        }
        for (label, key) in [
            ("新增", "added"),
            ("已删除", "removed"),
            ("内容变化", if section == "inputs" { "content_changed" } else { "changed" }),
            ("访问分类变化", "access_class_changed"),
        ] {
            if let Some(rows) = value[section][key].as_array() {
                for row in rows {
                    let path = row["path_display"].as_str().unwrap_or("?");
                    match key {
                        "content_changed" | "changed" => println!(
                            "{label}\t{path}\t{} -> {}",
                            row["a_sha256"].as_str().unwrap_or("?"),
                            row["b_sha256"].as_str().unwrap_or("?")
                        ),
                        "access_class_changed" => println!(
                            "{label}\t{path}\t{} -> {}",
                            row["a_access_class"].as_str().unwrap_or("?"),
                            row["b_access_class"].as_str().unwrap_or("?")
                        ),
                        _ => println!("{label}\t{path}"),
                    }
                }
            }
        }
    }
    let context = &value["run_context"];
    println!(
        "运行信息（不参与文件内容比较）：实验 {} -> {}；开始时间 {} -> {}；退出 {:?} -> {:?}",
        context["experiment"]["a"], context["experiment"]["b"],
        context["started_unix"]["a"], context["started_unix"]["b"],
        context["exit"]["a"], context["exit"]["b"]
    );
    let verdict = &value["verdict"];
    println!(
        "结论：输入内容一致={}；访问分类一致={}；输出一致={}",
        if verdict["inputs_content_identical"] == true { "是" } else { "否" },
        if verdict["inputs_access_identical"] == true { "是" } else { "否" },
        if verdict["outputs_identical"] == true { "是" } else { "否" }
    );
    if verdict["both_capture_complete"] != true {
        println!("注意：至少一份报告采集不完整，比较结果仅作参考。");
    }
}
fn compare_cli(args: &[OsString]) -> Result<(), String> {
    if args.len() < 2 || args.len() > 3 || (args.len() == 3 && args[2] != "--json") {
        return Err("用法：experiment compare <报告A目录> <报告B目录> [--json]".into());
    }
    let left = load_report(Path::new(&args[0]))?;
    let right = load_report(Path::new(&args[1]))?;
    let mut value = compare_reports(&left, &right)?;
    value["report_a"]["directory"] = json!(crate::history::display_path(args[0].as_bytes()));
    value["report_b"]["directory"] = json!(crate::history::display_path(args[1].as_bytes()));
    if args.len() == 3 {
        println!("{}", serde_json::to_string_pretty(&value).unwrap());
    } else {
        print_compare(&value);
    }
    Ok(())
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

struct RunArgs {
    mount: OsString,
    name: OsString,
    output: PathBuf,
    command: Vec<OsString>,
}
fn parse_run(args: &[OsString]) -> Result<RunArgs, String> {
    const USAGE: &str =
        "用法：experiment run <挂载点> <名称> --output <新报告目录> -- <命令> [参数...]";
    if args.len() < 5 {
        return Err(USAGE.into());
    }
    let mount = args[0].clone();
    let name = args[1].clone();
    let mut output = None;
    let mut index = 2;
    while index < args.len() {
        match args[index].to_str() {
            Some("--output") if index + 1 < args.len() => {
                if output.replace(PathBuf::from(&args[index + 1])).is_some() {
                    return Err("重复的 --output".into());
                }
                index += 2;
            }
            Some("--") => {
                let output = output.ok_or("缺少 --output")?;
                if index + 1 >= args.len() {
                    return Err("缺少实验命令".into());
                }
                return Ok(RunArgs {
                    mount,
                    name,
                    output,
                    command: args[index + 1..].to_vec(),
                });
            }
            _ => return Err(format!("未知或不完整的 experiment run 参数；{USAGE}")),
        }
    }
    Err("缺少 -- 和实验命令".into())
}
struct ReplayArgs {
    mount: OsString,
    original: PathBuf,
    name: OsString,
    output: PathBuf,
}
fn parse_replay(args: &[OsString]) -> Result<ReplayArgs, String> {
    const USAGE: &str = "用法：experiment replay <挂载点> <原报告目录> <新名称> --output <新报告目录>";
    if args.len() < 5 {
        return Err(USAGE.into());
    }
    let mount = args[0].clone();
    let original = PathBuf::from(&args[1]);
    let name = args[2].clone();
    let mut output = None;
    let mut index = 3;
    while index < args.len() {
        match args[index].to_str() {
            Some("--output") if index + 1 < args.len() => {
                if output.replace(PathBuf::from(&args[index + 1])).is_some() {
                    return Err("重复的 --output".into());
                }
                index += 2;
            }
            _ => return Err(format!("未知或不完整的 experiment replay 参数；{USAGE}")),
        }
    }
    Ok(ReplayArgs {
        mount,
        original,
        name,
        output: output.ok_or("缺少 --output")?,
    })
}
pub fn cli(args: &[OsString]) -> Result<(), String> {
    use crate::service::Command;
    match args.first().and_then(|s| s.to_str()) {
        Some("run") => {
            let parsed = parse_run(&args[1..])?;
            run(&parsed.mount, &parsed.name, &parsed.output, &parsed.command, None)
        }
        Some("replay") => {
            let parsed = parse_replay(&args[1..])?;
            let report = load_report(&parsed.original)?;
            if report["cancelled"] == true || report["signal"].is_number() {
                return Err("中断命令没有正常结束，不能自动作为完成实验重放".into());
            }
            verify(&parsed.mount, &parsed.original)?;
            let argv: Vec<Vec<u8>> = serde_json::from_value(report["command_bytes"].clone())
                .map_err(|_| "缺少原命令参数")?;
            let command: Vec<OsString> = argv.into_iter().map(OsString::from_vec).collect();
            if command.is_empty() {
                return Err("原命令为空".into());
            }
            run(
                &parsed.mount,
                &parsed.name,
                &parsed.output,
                &command,
                Some(report["snapshot"].as_str().unwrap().into()),
            )
        }
        Some("compare") => compare_cli(&args[1..]),
        Some("show") if args.len() == 2 => {
            let report = load_report(Path::new(&args[1]))?;
            println!("{}", serde_json::to_string_pretty(&report).unwrap());
            Ok(())
        }
        Some("verify") if args.len() == 3 => verify(&args[1], Path::new(&args[2])),
        Some("recover") if args.len() == 4 => {
            let name = args[2].to_str().ok_or("实验名称必须是 UTF-8")?;
            recover(&args[1], name, Path::new(&args[3]))
        }
        Some("restore") if args.len() == 4 => {
            let snapshot = snapshot_name(args[2].to_str().ok_or("实验名称必须是 UTF-8")?)
                .map_err(|e| format!("无效名称：{e}"))?;
            request(
                &args[1],
                Command::RestoreTree {
                    snapshot,
                    source: b".".to_vec(),
                    destination: args[3].as_bytes().to_vec(),
                    dry_run: false,
                },
            )?;
            println!("实验输入版本已恢复到新目录；等待后续同步。");
            Ok(())
        }
        Some("delete") if args.len() == 3 => {
            request(
                &args[1],
                Command::ExperimentDelete {
                    name: args[2].to_str().ok_or("实验名称必须是 UTF-8")?.into(),
                },
            )?;
            println!("已删除实验输入快照；外部报告仍保留，但不能再从此存储取回该版本。");
            Ok(())
        }
        _ => Err("用法：experiment run <挂载点> <名称> --output <新报告目录> -- <命令> [参数...]；或 show/verify/replay/compare/restore/delete/recover".into()),
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
        "environment":environment_summary(),
        "outputs":{"captured":false,"complete":false,"files":[],"count":0,"total_bytes":0,"scope":OUTPUT_SCOPE},
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
        capture_outputs(&mut report, &output);
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
        capture_outputs(&mut report, &output);
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
    println!("输入验证通过：{} 个文件与实验输入版本一致。", inputs.len());
    if let Some(outputs) = report.get("outputs") {
        if outputs["captured"] != true {
            return Err("报告没有采集输出文件摘要，不能作为完整输出证据验证".into());
        }
        if outputs["complete"] != true {
            let reasons = outputs["errors"]
                .as_array()
                .map(|rows| {
                    rows.iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join("；")
                })
                .unwrap_or_default();
            return Err(format!(
                "输出文件摘要不完整{}{}",
                if reasons.is_empty() { "" } else { "：" },
                reasons
            ));
        }
        let files = outputs["files"].as_array().ok_or("缺少输出文件清单")?;
        for entry in files {
            let relative: Vec<u8> = serde_json::from_value(entry["path_bytes"].clone())
                .map_err(|_| "无效输出路径")?;
            let path = dir.join(safe_relative_path(&relative)?);
            let (sha256, bytes) =
                hash_regular_file(&path).map_err(|error| format!("输出校验失败：{error}"))?;
            if entry["sha256"] != sha256 || entry["bytes"].as_u64() != Some(bytes) {
                return Err(format!(
                    "输出校验不一致：{}",
                    entry["path_display"].as_str().unwrap_or("?")
                ));
            }
        }
        println!(
            "输出验证通过：{} 个报告目录内文件与摘要一致。",
            files.len()
        );
    }
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
    #[test]
    fn output_digests_cover_regular_files_in_stable_order() {
        let dir = std::env::temp_dir().join(format!(
            "teamfs-outputs-{}-{}",
            std::process::id(),
            time::get_time().nsec
        ));
        std::fs::create_dir_all(dir.join("outputs/sub")).unwrap();
        std::fs::write(dir.join("outputs/z.txt"), b"z").unwrap();
        std::fs::write(dir.join("outputs/sub/a.bin"), b"\x00\xff").unwrap();
        std::fs::write(dir.join("stdout.txt"), b"out").unwrap();
        std::os::unix::fs::symlink("z.txt", dir.join("outputs/link")).unwrap();
        let value = output_digests(&dir).unwrap();
        assert_eq!(value["captured"], true);
        assert_eq!(value["complete"], true);
        assert_eq!(value["count"], 3);
        let paths: Vec<&str> = value["files"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["path_display"].as_str().unwrap())
            .collect();
        assert_eq!(
            paths,
            ["outputs/sub/a.bin", "outputs/z.txt", "stdout.txt"]
        );
        assert_eq!(value["files"][0]["bytes"], 2);
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn compare_ignores_run_metadata_and_reports_file_content_changes() {
        let base = json!({
            "schema_version":1, "experiment":"baseline", "snapshot":"experiment.baseline",
            "capture_complete":true, "started_unix":100, "finished_unix":110,
            "process_group":42, "exit_code":0, "signal":Value::Null, "outcome":"succeeded",
            "command_display":["python3","analyze.py"],
            "capture":{"inputs":[
                {"path_bytes":b"data.csv".to_vec(),"path_display":"data.csv","sha256":"aaa",
                 "access_class":"content_read","read_calls":1,"read_bytes":10,"pids":[42]},
                {"path_bytes":b"code.py".to_vec(),"path_display":"code.py","sha256":"bbb",
                 "access_class":"content_read","read_calls":1,"read_bytes":5,"pids":[42]}
            ]},
            "outputs":{"captured":true,"complete":true,"files":[
                {"path_bytes":b"outputs/result.json".to_vec(),
                 "path_display":"outputs/result.json","sha256":"111","bytes":7}
            ]}
        });
        let mut metadata_only = base.clone();
        metadata_only["experiment"] = json!("renamed");
        metadata_only["snapshot"] = json!("experiment.renamed");
        metadata_only["started_unix"] = json!(200);
        metadata_only["finished_unix"] = json!(250);
        metadata_only["process_group"] = json!(99);
        metadata_only["capture"]["inputs"][0]["read_calls"] = json!(999);
        metadata_only["capture"]["inputs"][0]["pids"] = json!([99]);
        let comparison = compare_reports(&base, &metadata_only).unwrap();
        assert_eq!(comparison["inputs"]["summary"]["content_changed"], 0);
        assert_eq!(comparison["inputs"]["summary"]["added"], 0);
        assert_eq!(comparison["inputs"]["summary"]["removed"], 0);
        assert_eq!(comparison["outputs"]["summary"]["changed"], 0);
        assert_eq!(comparison["verdict"]["inputs_content_identical"], true);
        assert_eq!(comparison["verdict"]["outputs_identical"], true);
        assert_eq!(comparison["run_context"]["experiment"]["changed"], true);
        assert_eq!(comparison["run_context"]["started_unix"]["changed"], true);
        assert_eq!(comparison["run_context"]["process_group"]["changed"], true);

        let mut changed = base.clone();
        changed["capture"]["inputs"][0]["sha256"] = json!("ccc");
        changed["outputs"]["files"][0]["sha256"] = json!("222");
        let comparison = compare_reports(&base, &changed).unwrap();
        assert_eq!(comparison["inputs"]["summary"]["content_changed"], 1);
        assert_eq!(
            comparison["inputs"]["content_changed"][0]["path_display"],
            "data.csv"
        );
        assert_eq!(comparison["outputs"]["summary"]["changed"], 1);
        assert_eq!(comparison["verdict"]["inputs_content_identical"], false);
        assert_eq!(comparison["verdict"]["outputs_identical"], false);

        let mut membership = base.clone();
        membership["capture"]["inputs"] = json!([
            {"path_bytes":b"code.py".to_vec(),"path_display":"code.py","sha256":"bbb",
             "access_class":"content_read"},
            {"path_bytes":b"new.csv".to_vec(),"path_display":"new.csv","sha256":"ddd",
             "access_class":"content_read"}
        ]);
        membership["outputs"]["files"] = json!([
            {"path_bytes":b"outputs/new.txt".to_vec(),
             "path_display":"outputs/new.txt","sha256":"333","bytes":3}
        ]);
        let comparison = compare_reports(&base, &membership).unwrap();
        assert_eq!(comparison["inputs"]["summary"]["added"], 1);
        assert_eq!(comparison["inputs"]["summary"]["removed"], 1);
        assert_eq!(
            comparison["inputs"]["added"][0]["path_display"],
            "new.csv"
        );
        assert_eq!(
            comparison["inputs"]["removed"][0]["path_display"],
            "data.csv"
        );
        assert_eq!(comparison["outputs"]["summary"]["added"], 1);
        assert_eq!(comparison["outputs"]["summary"]["removed"], 1);
        assert_eq!(
            comparison["outputs"]["added"][0]["path_display"],
            "outputs/new.txt"
        );
        assert_eq!(
            comparison["outputs"]["removed"][0]["path_display"],
            "outputs/result.json"
        );
    }
}
