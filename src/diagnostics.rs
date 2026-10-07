//! Read-only offline inspection and environment diagnostics. Never repair or mount implicitly.
use serde_json::{json, Value};
use std::{
    ffi::OsString,
    fs::OpenOptions,
    os::unix::{ffi::OsStrExt, fs::OpenOptionsExt, io::AsRawFd},
    path::{Component, Path, PathBuf},
};

fn report(kind: &str) -> Value {
    json!({"schema_version":1,"tool":"TeamFS","version":env!("CARGO_PKG_VERSION"),"kind":kind,"ok":true,"checks":[]})
}
fn add(report: &mut Value, name: &str, state: &str, detail: &str, action: &str) {
    if state == "error" {
        report["ok"] = json!(false);
    }
    report["checks"]
        .as_array_mut()
        .unwrap()
        .push(json!({"name":name,"status":state,"detail":detail,"action":action}));
}
fn print_report(value: Value, json_output: bool) -> Result<(), String> {
    if json_output {
        println!("{}", serde_json::to_string_pretty(&value).unwrap());
    } else {
        println!(
            "TeamFS {} — {}",
            value["kind"].as_str().unwrap(),
            if value["ok"] == true {
                "检查完成"
            } else {
                "发现问题"
            }
        );
        for c in value["checks"].as_array().unwrap() {
            println!(
                "[{}] {}：{}",
                c["status"].as_str().unwrap(),
                c["name"].as_str().unwrap(),
                c["detail"].as_str().unwrap()
            );
            if c["action"] != "" {
                println!("  建议：{}", c["action"].as_str().unwrap());
            }
        }
        if !value["storage"].is_null() {
            println!(
                "存储统计：{}",
                serde_json::to_string_pretty(&value["storage"]).unwrap()
            );
        }
    }
    if value["ok"] == true {
        Ok(())
    } else {
        Err("诊断未通过；请按检查报告处理后重试（未执行修复）".into())
    }
}

pub fn check_cli(args: &[OsString]) -> Result<(), String> {
    if args.is_empty() || args.len() > 2 || args.len() == 2 && args[1] != "--json" {
        return Err("用法：teamfs check <存储目录> [--json]；请先正常卸载".into());
    }
    let mut r = report("check");
    let dir = Path::new(&args[0]);
    r["store_path_bytes"] = json!(dir.as_os_str().as_bytes());
    r["store_path_display"] = json!(crate::history::display_path(dir.as_os_str().as_bytes()));
    let checked = (|| -> Result<Value, String> {
        // Open the existing lock only: diagnostics must never initialize a new store.
        let lock = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(dir.join("store.lock"))
            .map_err(|e| format!("无法打开已有 store.lock：{e}"))?;
        if !lock.metadata().map_err(|e| e.to_string())?.is_file() {
            return Err("store.lock 不是普通文件".into());
        }
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err("存储正在使用，不能进行离线自检；请先同步并正常卸载".into());
        }
        crate::store::Store::inspect(&dir.join("state.sqlite3"))
    })();
    match checked {
        Ok(stats) => {
            add(
                &mut r,
                "database",
                "ok",
                "SQLite integrity_check、格式、业务树、快照、回收记录及内容引用检查通过",
                "",
            );
            if stats["format"].as_i64().unwrap_or(3) < 3 {
                add(
                    &mut r,
                    "format",
                    "warning",
                    "旧格式有效；自检没有执行迁移",
                    "正式升级挂载前保留一份独立备份",
                );
            }
            if stats["unreferenced_blob_bytes"].as_u64().unwrap_or(0) > 0 {
                add(
                    &mut r,
                    "space",
                    "warning",
                    "存在无引用内容；这些内容不等于数据库损坏",
                    "正常重新挂载会回收无引用 blobs；备份会生成紧凑副本",
                );
            }
            add(
                &mut r,
                "content_assurance",
                "info",
                "本检查验证结构与引用，不证明内容与某个历史原稿逐字节一致",
                "已有备份可用 backup verify 校验 SHA256；自检不会自动修复",
            );
            r["storage"] = stats;
        }
        Err(e) => add(
            &mut r,
            "database",
            "error",
            &e,
            "确认目录正确并已卸载；保留原存储，必要时用已验证备份导入新目录",
        ),
    }
    print_report(r, args.len() == 2)
}

fn executable(name: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::env::var_os("PATH").map_or(false, |p| {
        std::env::split_paths(&p).any(|dir| {
            dir.join(name).metadata().map_or(false, |m| {
                m.is_file() && m.permissions().mode() & 0o111 != 0
            })
        })
    })
}
fn absolute(path: &Path) -> Result<PathBuf, String> {
    let raw = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()
            .map_err(|e| e.to_string())?
            .join(path)
    };
    let mut out = PathBuf::new();
    for c in raw.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => (),
            _ => out.push(c.as_os_str()),
        }
    }
    Ok(out)
}
fn unescape_mount(s: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < s.len() {
        if s[i] == b'\\'
            && i + 3 < s.len()
            && s[i + 1..i + 4].iter().all(|b| (b'0'..=b'7').contains(b))
        {
            out.push((s[i + 1] - b'0') * 64 + (s[i + 2] - b'0') * 8 + s[i + 3] - b'0');
            i += 4;
        } else {
            out.push(s[i]);
            i += 1;
        }
    }
    out
}
pub fn doctor_cli(args: &[OsString]) -> Result<(), String> {
    let mut mount = None;
    let mut store = None;
    let mut json_output = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].to_str() {
            Some("--json") if !json_output => json_output = true,
            Some("--store") if store.is_none() && i + 1 < args.len() => {
                i += 1;
                store = Some(PathBuf::from(&args[i]));
            }
            _ if !args[i].as_bytes().starts_with(b"-") && mount.is_none() => {
                mount = Some(PathBuf::from(&args[i]))
            }
            _ => return Err("用法：teamfs doctor [挂载点] [--store <存储目录>] [--json]".into()),
        }
        i += 1;
    }
    let mut r = report("doctor");
    match OpenOptions::new().read(true).write(true).open("/dev/fuse") {
        Ok(_) => add(
            &mut r,
            "fuse_device",
            "ok",
            "当前用户可以打开 /dev/fuse",
            "",
        ),
        Err(e) => add(
            &mut r,
            "fuse_device",
            "error",
            &e.to_string(),
            "在启用 FUSE 的 Linux/WSL2 中运行并检查设备访问权限",
        ),
    }
    if executable("fusermount3") || executable("fusermount") {
        add(
            &mut r,
            "unmount_helper",
            "ok",
            "已找到 fusermount3/fusermount",
            "",
        );
    } else {
        add(
            &mut r,
            "unmount_helper",
            "error",
            "缺少卸载工具",
            "安装发行版提供的 fuse3 或 fuse 工具",
        );
    }
    for name in ["git", "rsync", "vim", "fio"] {
        add(
            &mut r,
            &format!("optional_{name}"),
            if executable(name) { "ok" } else { "warning" },
            if executable(name) {
                "已找到可执行文件"
            } else {
                "未安装；不影响 TeamFS 挂载，但相应验收或性能测试不可用"
            },
            "",
        );
    }
    if let Some(mount) = mount {
        let target = absolute(&mount)?;
        // Inspect mountinfo without touching a possibly hung FUSE mount.
        let data = std::fs::read("/proc/self/mountinfo").map_err(|e| e.to_string())?;
        let found = data.split(|b| *b == b'\n').find(|line| {
            line.split(|b| *b == b' ').nth(4).map_or(false, |p| {
                unescape_mount(p) == target.as_os_str().as_bytes()
            })
        });
        match found {
            Some(line)
                if line
                    .windows(b" - fuse.teamfs ".len())
                    .any(|w| w == b" - fuse.teamfs ") =>
            {
                add(&mut r, "mount", "ok", "内核挂载表中存在 TeamFS 挂载", "");
                add(
                    &mut r,
                    "liveness",
                    "info",
                    "未读取挂载目录：挂载表存在不代表服务响应正常",
                    "用真实监控查看是否有新采样；失效挂载须先确认原进程已退出再卸载",
                );
            }
            Some(_) => add(
                &mut r,
                "mount",
                "error",
                "该路径挂载的是其他文件系统",
                "选择 TeamFS 的实际挂载点",
            ),
            None => add(
                &mut r,
                "mount",
                "warning",
                "该路径未作为独立挂载点出现在挂载表中（不解析软链接）",
                "检查启动输出中的实际挂载路径",
            ),
        }
        if let Some(ref dir) = store {
            let dir = absolute(dir)?;
            if dir.starts_with(&target) {
                add(
                    &mut r,
                    "store_location",
                    "error",
                    "存储目录位于挂载路径内部",
                    "将存储目录放在挂载点之外",
                );
                return print_report(r, json_output);
            }
        }
    }
    if let Some(dir) = store {
        match OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(dir.join("store.lock"))
        {
            Ok(lock) => {
                let busy =
                    unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0;
                add(
                    &mut r,
                    "store_lock",
                    if busy { "warning" } else { "ok" },
                    if busy {
                        "存储被其他进程占用；已挂载时属于正常情况"
                    } else {
                        "现有存储锁可用；尚未检查数据库内容"
                    },
                    "完整离线检查使用 teamfs check <存储目录>",
                );
            }
            Err(e) => add(
                &mut r,
                "store_lock",
                "warning",
                &e.to_string(),
                "确认是否为已有 TeamFS 存储；doctor 不创建目录或锁文件",
            ),
        }
    }
    print_report(r, json_output)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mountinfo_paths_decode_without_utf8_loss() {
        assert_eq!(unescape_mount(b"/a\\040b/\\134x\xff"), b"/a b/\\x\xff");
        assert_eq!(
            absolute(Path::new("/a/b/../c/.")).unwrap(),
            Path::new("/a/c")
        );
    }
}
