mod audit;
mod autosync;
mod backup;
mod content;
mod diagnostics;
mod filesystem;
mod history;
mod metrics;
mod model;
mod service;
mod store;
use service::{Command, Service};
use std::{
    ffi::{OsStr, OsString},
    fs::OpenOptions,
    io::{Read, Seek, SeekFrom, Write},
    os::unix::ffi::{OsStrExt, OsStringExt},
    path::PathBuf,
    sync::{Arc, Mutex},
};

fn main() {
    if let Err(error) = run() {
        eprintln!("TeamFS: {error}");
        std::process::exit(1);
    }
}
fn help() {
    println!(
        r#"TeamFS 0.6 — 可保存、可恢复的用户空间文件系统

teamfs mount <挂载点> [--store <目录> | --memory] [--auto-sync <秒数>] [--capacity-mib <MiB>] [--max-file-mib <MiB>] [--cached-io] [--audit-log <外部日志路径> | --no-audit-file] [--trace]
teamfs logs <挂载点> [--json] [--errors] [--operation <操作>] [--path <路径片段>] [--since <序号>]
teamfs metrics <挂载点> [--json]
teamfs check <存储目录> [--json]
teamfs doctor [挂载点] [--store <存储目录>] [--json]
teamfs sync <挂载点>
teamfs snapshot create <挂载点> <名称>
teamfs snapshot list <挂载点>
teamfs snapshot delete <挂载点> <名称>
teamfs snapshot diff <挂载点> <名称> [--json]
teamfs restore <挂载点> <快照> <源相对路径> <目标相对路径>
teamfs trash list <挂载点> [--json]
teamfs trash restore <挂载点> <记录ID> <目标相对路径>
teamfs trash purge <挂载点> <记录ID> | --all
teamfs restore-tree <挂载点> <快照> <源目录或 .> <新目标目录> [--dry-run] [--json]
teamfs backup create <挂载点> <新备份目录>
teamfs backup verify <备份目录>
teamfs backup import <备份目录> --store <新存储目录>

默认持久化；普通修改在 sync/fsync/正常卸载时保存。异常退出只恢复最后的成功同步。
--auto-sync 接受 1～86400 秒，默认关闭，仅持久化模式可用；间隔不是最大丢失时间保证。
回收站保护删除和改名覆盖，最多 1024 条/64 MiB，满时拒绝删除，请先显式清理。
回收记录与删除一起同步；直接写入或截断由快照保护。单文件/软链接恢复和整目录恢复都要求目标不存在、父目录已存在。
创建/删除快照、清理回收站会同步全部当前状态。恢复不覆盖、不移除历史，之后需要同步。
目录恢复先预检，失败不留下半成品；备份包含数据库及 SHA256 清单，导入不覆盖已有存储。
容量默认 64 MiB/单文件16 MiB，可配置总量至 4096 MiB、单文件至64 MiB；读取缓存上限8 MiB。
--cached-io 为业务文件启用内核缓存和 mmap；快照/备份前先 fsync/msync 应用尚未提交的写入。
结构化日志可通过 .teamfs/events.json 或 logs 查询；持久化模式默认写入存储目录的 logs/events.jsonl。
日志为尽力记录，写入失败或队列繁忙不会拒绝业务操作；--no-audit-file 只关闭文件日志。
--memory 是教学内存模式，退出后包括快照、回收站在内的所有数据丢失。
"#
    );
}
fn name(arg: &OsStr) -> Result<String, String> {
    arg.to_str()
        .map(str::to_owned)
        .ok_or("快照名称必须是 UTF-8".into())
}
fn status(mount: &OsStr) -> Result<serde_json::Value, String> {
    let path = PathBuf::from(mount).join(".teamfs/status.json");
    let value: serde_json::Value =
        serde_json::from_slice(&std::fs::read(path).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    if value["filesystem"] != "TeamFS" || value["schema_version"] != 1 {
        return Err("不是支持的 TeamFS 挂载点".into());
    }
    Ok(value)
}
fn send(mount: &OsStr, command: Command) -> Result<(), String> {
    status(mount)?;
    let mut file = OpenOptions::new()
        .write(true)
        .open(PathBuf::from(mount).join(".teamfs/control"))
        .map_err(|e| e.to_string())?;
    file.write_all(&serde_json::to_vec(&command).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    file.sync_all().map_err(|e| {
        format!("管理操作失败：{e}；如为同步错误，请查看 .teamfs/status.json 的 last_sync_error")
    })?;
    println!("操作完成（模式：{}）", status(mount)?["mode"]);
    if matches!(
        command,
        Command::Restore { .. } | Command::TrashRestore { .. }
    ) {
        println!(
            "文件已恢复到新路径；后续成功同步（手动、已启用的自动同步或正常卸载）后才能持久保存。"
        );
    }
    Ok(())
}
fn run() -> Result<(), String> {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    if args.is_empty() || args[0] == "--help" || args[0] == "-h" {
        help();
        return Ok(());
    }
    match args[0].to_str() {
        Some("check") => diagnostics::check_cli(&args[1..]),
        Some("doctor") => diagnostics::doctor_cli(&args[1..]),
        Some("metrics") => metrics_cli(&args[1..]),
        Some("logs") if args.len() >= 2 => logs(&args[1..]),
        Some("restore-tree")
            if args.len() >= 5 && args[5..].iter().all(|s| s == "--dry-run" || s == "--json") =>
        {
            let dry_run = args[5..].iter().any(|s| s == "--dry-run");
            status(&args[1])?;
            let cmd = Command::RestoreTree {
                snapshot: name(&args[2])?,
                source: args[3].as_bytes().to_vec(),
                destination: args[4].as_bytes().to_vec(),
                dry_run,
            };
            let mut file = OpenOptions::new()
                .read(true)
                .write(true)
                .open(PathBuf::from(&args[1]).join(".teamfs/control"))
                .map_err(|e| e.to_string())?;
            file.write_all(&serde_json::to_vec(&cmd).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
            file.sync_all().map_err(|e| e.to_string())?;
            file.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes).map_err(|e| e.to_string())?;
            let preview: serde_json::Value =
                serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
            if args[5..].iter().any(|s| s == "--json") {
                println!("{}", serde_json::to_string_pretty(&preview).unwrap());
            } else {
                println!(
                    "{}：文件 {}，目录 {}，软链接 {}，内容 {} 字节；可恢复={}；冲突/限制 errno={}",
                    if dry_run {
                        "恢复预览"
                    } else {
                        "恢复完成"
                    },
                    preview["files"],
                    preview["directories"],
                    preview["symlinks"],
                    preview["bytes"],
                    preview["can_restore"],
                    preview["error_errno"]
                );
                for e in preview["entries"].as_array().ok_or("invalid response")? {
                    println!(
                        "{}\t{}",
                        e["kind"],
                        e["path_display"].as_str().unwrap_or("")
                    );
                }
                if !dry_run {
                    println!("已恢复到新目录，等待下一次成功同步；原目录和历史保持不变。");
                }
            }
            Ok(())
        }
        Some("backup") if args.len() == 4 && args[1] == "create" => {
            let mount = PathBuf::from(&args[2])
                .canonicalize()
                .map_err(|e| e.to_string())?;
            let target = backup::target_path(std::path::Path::new(&args[3]))?;
            if target.starts_with(&mount) {
                return Err("备份目标必须位于挂载点之外".into());
            }
            let state = status(&args[2])?;
            let source: Vec<u8> = serde_json::from_value(state["store_path_bytes"].clone())
                .map_err(|_| "备份仅支持持久化模式")?;
            send(&args[2], Command::Sync)?;
            let manifest = backup::create(&PathBuf::from(OsString::from_vec(source)), &target)?;
            println!(
                "备份完成：{}\n{}",
                target.display(),
                serde_json::to_string_pretty(&manifest).unwrap()
            );
            Ok(())
        }
        Some("backup") if args.len() == 3 && args[1] == "verify" => {
            println!(
                "{}",
                serde_json::to_string_pretty(&backup::verify(std::path::Path::new(&args[2]))?)
                    .unwrap()
            );
            Ok(())
        }
        Some("backup") if args.len() == 5 && args[1] == "import" && args[3] == "--store" => {
            backup::import(
                std::path::Path::new(&args[2]),
                std::path::Path::new(&args[4]),
            )?;
            println!("已导入新的存储目录，可通过 mount --store 挂载。");
            Ok(())
        }
        Some("mount") => mount(&args[1..]),
        Some("sync") if args.len() == 2 => send(&args[1], Command::Sync),
        Some("trash")
            if (args.len() == 3 || args.len() == 4 && args[3] == "--json") && args[1] == "list" =>
        {
            query(&args[2], "trash/index.json", args.len() == 4, false)
        }
        Some("trash") if args.len() == 5 && args[1] == "restore" => send(
            &args[2],
            Command::TrashRestore {
                id: record_id(&args[3])?,
                destination: args[4].as_bytes().to_vec(),
            },
        ),
        Some("trash") if args.len() == 4 && args[1] == "purge" => send(
            &args[2],
            if args[3] == "--all" {
                Command::TrashPurgeAll
            } else {
                Command::TrashPurge {
                    id: record_id(&args[3])?,
                }
            },
        ),
        Some("snapshot")
            if (args.len() == 4 || args.len() == 5 && args[4] == "--json") && args[1] == "diff" =>
        {
            let name = name(&args[3])?;
            model::MemFs::valid_name(OsStr::new(&name)).map_err(|_| "快照名称无效")?;
            query(&args[2], &format!("diffs/{name}"), args.len() == 5, true)
        }
        Some("snapshot") if args.len() == 3 && args[1] == "list" => {
            println!(
                "{}",
                serde_json::to_string_pretty(&status(&args[2])?["snapshots"])
                    .map_err(|e| e.to_string())?
            );
            Ok(())
        }
        Some("snapshot") if args.len() == 4 && args[1] == "create" => send(
            &args[2],
            Command::SnapshotCreate {
                name: name(&args[3])?,
            },
        ),
        Some("snapshot") if args.len() == 4 && args[1] == "delete" => send(
            &args[2],
            Command::SnapshotDelete {
                name: name(&args[3])?,
            },
        ),
        Some("restore") if args.len() == 5 => send(
            &args[1],
            Command::Restore {
                snapshot: name(&args[2])?,
                source: args[3].as_bytes().to_vec(),
                destination: args[4].as_bytes().to_vec(),
            },
        ),
        _ => Err("参数无效；使用 teamfs --help 查看用法".into()),
    }
}
fn record_id(arg: &OsStr) -> Result<u64, String> {
    arg.to_str()
        .filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
        .and_then(|s| s.parse::<u64>().ok())
        .filter(|id| *id > 0 && *id < i64::MAX as u64)
        .ok_or("记录 ID 必须是正整数".into())
}
fn query(mount: &OsStr, relative: &str, json: bool, diff: bool) -> Result<(), String> {
    status(mount)?;
    let value: serde_json::Value = serde_json::from_slice(
        &std::fs::read(PathBuf::from(mount).join(".teamfs").join(relative))
            .map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&value).map_err(|e| e.to_string())?
        );
    } else if diff {
        for entry in value["changes"].as_array().ok_or("差异格式无效")? {
            let label = match entry["change"].as_str().unwrap_or("") {
                "added" => "新增",
                "removed" => "已删除",
                "type_changed" => "类型变化",
                _ if entry["content_changed"] == true => "内容修改",
                _ => "属性修改",
            };
            println!(
                "{label}\t{}\t属性={}",
                entry["path_display"].as_str().unwrap_or(""),
                entry["metadata_changed"]
            );
        }
        println!("变化汇总：{}", value["summary"]);
    } else {
        for entry in value["entries"].as_array().ok_or("回收站格式无效")? {
            println!(
                "{}\t{}\t{} 字节\t{}\t删除时间={}",
                entry["id"],
                if entry["reason"] == "unlink" {
                    "删除"
                } else {
                    "改名覆盖"
                },
                entry["size"],
                entry["path_display"].as_str().unwrap_or(""),
                entry["deleted_unix"]
            );
        }
        println!(
            "共 {} 条记录；恢复不会覆盖已有文件。",
            value["entries"].as_array().unwrap().len()
        );
    }
    Ok(())
}
fn logs(args: &[OsString]) -> Result<(), String> {
    status(&args[0])?;
    let mut errors = false;
    let mut json_output = false;
    let mut operation = None;
    let mut path = None;
    let mut since = 0;
    let mut i = 1;
    while i < args.len() {
        match args[i].to_str() {
            Some("--errors") => errors = true,
            Some("--json") => json_output = true,
            Some("--operation" | "--path" | "--since") if i + 1 < args.len() => {
                let flag = args[i].clone();
                i += 1;
                if flag == "--operation" {
                    operation = Some(args[i].to_str().ok_or("操作名必须为 UTF-8")?.to_owned());
                } else if flag == "--path" {
                    path = Some(history::display_path(args[i].as_bytes()));
                } else {
                    since = args[i]
                        .to_str()
                        .and_then(|s| s.parse::<u64>().ok())
                        .ok_or("--since 必须是非负整数")?;
                }
            }
            _ => return Err("logs 参数无效".into()),
        }
        i += 1;
    }
    let mut value: serde_json::Value = serde_json::from_slice(
        &std::fs::read(PathBuf::from(&args[0]).join(".teamfs/events.json"))
            .map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    value["events"]
        .as_array_mut()
        .ok_or("日志格式无效")?
        .retain(|e| {
            e["seq"].as_u64().unwrap_or(0) > since
                && (!errors || e["result"] == "error")
                && operation
                    .as_ref()
                    .map_or(true, |op| e["operation"].as_str() == Some(op.as_str()))
                && path.as_ref().map_or(true, |p| {
                    [&e["path_display"], &e["destination_display"]]
                        .iter()
                        .any(|v| v.as_str().map_or(false, |v| v.contains(p)))
                })
        });
    if json_output {
        println!("{}", serde_json::to_string_pretty(&value).unwrap());
    } else {
        println!(
            "会话 {}；本次挂载已记录 {} 项；最近窗口移出 {} 项。",
            value["summary"]["session_id"],
            value["summary"]["last_seq"],
            value["summary"]["evicted"]
        );
        for e in value["events"].as_array().unwrap() {
            println!(
                "#{}\t{}\t{}\t{}\tPID={}\t{} us\t{}",
                e["seq"],
                e["operation"].as_str().unwrap_or(""),
                e["path_display"].as_str().unwrap_or("—"),
                e["result"].as_str().unwrap_or(""),
                e["caller"]["pid"],
                e["duration_us"],
                e["error_message"].as_str().unwrap_or("")
            );
        }
    }
    Ok(())
}
fn mount(args: &[OsString]) -> Result<(), String> {
    if args.is_empty() {
        return Err("缺少挂载点".into());
    }
    let mut trace = false;
    let mut memory = false;
    let mut store = None;
    let mut auto_sync = None;
    let mut cached_io = false;
    let mut capacity = None;
    let mut max_file = None;
    let mut audit_log = None;
    let mut no_audit_file = false;
    let mut i = 1;
    while i < args.len() {
        match args[i].to_str() {
            Some("--trace") => trace = true,
            Some("--memory") => memory = true,
            Some("--cached-io") => cached_io = true,
            Some("--no-audit-file") => no_audit_file = true,
            Some("--audit-log") if i + 1 < args.len() => {
                i += 1;
                if audit_log.replace(PathBuf::from(&args[i])).is_some() {
                    return Err("重复的 --audit-log".into());
                }
            }
            Some("--capacity-mib" | "--max-file-mib") if i + 1 < args.len() => {
                let total = args[i] == "--capacity-mib";
                i += 1;
                let value = args[i]
                    .to_str()
                    .filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
                    .and_then(|s| s.parse::<usize>().ok())
                    .and_then(|n| n.checked_mul(1024 * 1024))
                    .ok_or("容量参数必须为正整数 MiB")?;
                let slot = if total { &mut capacity } else { &mut max_file };
                if slot.replace(value).is_some() {
                    return Err("重复的容量参数".into());
                }
                if value == 0
                    || value
                        > if total {
                            store::HARD_CAPACITY
                        } else {
                            store::HARD_MAX_FILE
                        }
                {
                    return Err("容量超出范围".into());
                }
            }
            Some("--auto-sync") if i + 1 < args.len() => {
                i += 1;
                if auto_sync.is_some() {
                    return Err("重复的 --auto-sync".into());
                }
                let value = args[i]
                    .to_str()
                    .filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
                    .and_then(|s| s.parse::<u64>().ok())
                    .filter(|n| (1..=86400).contains(n))
                    .ok_or("--auto-sync 必须为 1～86400 的整数秒数")?;
                auto_sync = Some(value);
            }
            Some("--store") if i + 1 < args.len() => {
                i += 1;
                if store.is_some() {
                    return Err("重复的 --store".into());
                }
                store = Some(PathBuf::from(&args[i]));
            }
            _ => return Err("未知或不完整的 mount 参数".into()),
        }
        i += 1;
    }
    if memory && store.is_some() {
        return Err("--memory 与 --store 不能同时使用".into());
    }
    if memory && auto_sync.is_some() {
        return Err("--memory 不支持自动同步".into());
    }
    if no_audit_file && audit_log.is_some() {
        return Err("--no-audit-file 与 --audit-log 不能同时使用".into());
    }
    let mountpoint = PathBuf::from(&args[0])
        .canonicalize()
        .map_err(|e| format!("挂载目录无效：{e}"))?;
    if !mountpoint.is_dir()
        || std::fs::read_dir(&mountpoint)
            .map_err(|e| e.to_string())?
            .next()
            .is_some()
    {
        return Err("挂载点必须是未使用的空目录".into());
    }
    let store = if memory {
        None
    } else {
        let dir = store.unwrap_or(
            PathBuf::from(std::env::var_os("HOME").ok_or("HOME 未设置")?)
                .join(".local/share/teamfs/default"),
        );
        let absolute = if dir.is_absolute() {
            dir
        } else {
            std::env::current_dir()
                .map_err(|e| e.to_string())?
                .join(dir)
        };
        let mut resolved = PathBuf::new();
        for part in absolute.components() {
            match part {
                std::path::Component::ParentDir => {
                    resolved.pop();
                }
                std::path::Component::CurDir => (),
                _ => {
                    resolved.push(part.as_os_str());
                    if resolved.exists() {
                        resolved = resolved.canonicalize().map_err(|e| e.to_string())?;
                    }
                }
            }
        }
        if resolved.starts_with(&mountpoint) {
            return Err("存储目录必须在挂载点之外".into());
        }
        std::fs::create_dir_all(&resolved).map_err(|e| e.to_string())?;
        let resolved = resolved.canonicalize().map_err(|e| e.to_string())?;
        if resolved.starts_with(&mountpoint) {
            return Err("存储目录必须在挂载点之外".into());
        }
        Some(resolved)
    };
    let service = Arc::new(Mutex::new(Service::new(
        unsafe { libc::geteuid() },
        unsafe { libc::getegid() },
        store.as_deref(),
    )?));
    service
        .lock()
        .map_err(|_| "服务锁损坏")?
        .configure_auto_sync(auto_sync);
    service
        .lock()
        .map_err(|_| "服务锁损坏")?
        .configure_limits(capacity, max_file)?;
    let log_path = if no_audit_file {
        None
    } else {
        audit_log.or_else(|| store.as_ref().map(|p| p.join("logs/events.jsonl")))
    };
    if let Some(path) = log_path {
        let absolute = if path.is_absolute() {
            path
        } else {
            std::env::current_dir()
                .map_err(|e| e.to_string())?
                .join(path)
        };
        let mut resolved = PathBuf::new();
        for part in absolute.components() {
            match part {
                std::path::Component::ParentDir => {
                    resolved.pop();
                }
                std::path::Component::CurDir => (),
                _ => {
                    resolved.push(part.as_os_str());
                    if resolved.exists() {
                        resolved = resolved.canonicalize().map_err(|e| e.to_string())?;
                    }
                }
            }
        }
        if resolved.starts_with(&mountpoint) {
            return Err("日志文件必须位于挂载点之外，避免日志递归".into());
        }
        service
            .lock()
            .map_err(|_| "服务锁损坏")?
            .audit
            .set_file(resolved);
    }
    eprintln!(
        "TeamFS 挂载：{}；模式={}；存储={:?}",
        mountpoint.display(),
        service.lock().map_err(|_| "服务锁损坏")?.mode(),
        store
    );
    eprintln!("普通修改在同步或正常卸载时保存。异常退出会丢失未同步修改。");
    let options: Vec<&OsStr> = [
        "-o",
        "fsname=teamfs",
        "-o",
        "subtype=teamfs",
        "-o",
        "default_permissions",
    ]
    .iter()
    .map(OsStr::new)
    .collect();
    let mut session = fuse::Session::new(
        filesystem::TeamFs::new(service.clone(), trace, cached_io),
        &mountpoint,
        &options,
    )
    .map_err(|e| format!("挂载失败：{e}"))?;
    service
        .lock()
        .map_err(|_| "服务锁损坏")?
        .lifecycle("mount", None);
    let timer = auto_sync
        .map(|seconds| autosync::AutoSync::start(service.clone(), seconds))
        .transpose()?;
    eprintln!("自动同步间隔：{auto_sync:?} 秒；回收站保护删除和改名覆盖。");
    let mounted = session.run();
    let stopped = match timer {
        Some(timer) => timer.stop(),
        None => Ok(()),
    };
    // 不依赖 FUSE destroy 或 Drop：挂载循环结束后显式检查保存结果。
    let saved = service
        .lock()
        .map_err(|_| "服务锁损坏")?
        .sync_named("unmount");
    service
        .lock()
        .map_err(|_| "服务锁损坏")?
        .lifecycle("unmount", saved.err());
    if saved.is_err() {
        return Err(format!(
            "退出保存失败：{}",
            service
                .lock()
                .map_err(|_| "服务锁损坏")?
                .last_sync_error
                .as_deref()
                .unwrap_or("unknown")
        ));
    }
    stopped?;
    mounted.map_err(|e| format!("挂载失败：{e}"))?;
    eprintln!("TeamFS 已正常卸载；persistent 模式已同步，memory 模式数据已结束。");
    Ok(())
}

fn metrics_cli(args: &[OsString]) -> Result<(), String> {
    if args.is_empty() || args.len() > 2 || args.len() == 2 && args[1] != "--json" {
        return Err("用法：teamfs metrics <挂载点> [--json]".into());
    }
    status(&args[0])?;
    let value: serde_json::Value = serde_json::from_slice(
        &std::fs::read(PathBuf::from(&args[0]).join(".teamfs/metrics.json"))
            .map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    if args.len() == 2 {
        println!("{}", serde_json::to_string_pretty(&value).unwrap());
    } else {
        println!("本次挂载统计；P50/P95/P99 为各操作最近 256 次样本；单位微秒。保存时间包含在业务处理时间中。\n仅测量用户空间回调，不含内核排队和回复传递；嵌套 commit 与外层 fsync 不可相加。");
        for o in value["operations"].as_array().ok_or("指标格式无效")? {
            println!("{} 次数={} 错误={} P50={} P95={} P99={} 最大={} 锁等待总计={} 业务总计={} 保存总计={}",o["operation"].as_str().unwrap_or(""),o["calls"],o["errors"],o["p50_us"],o["p95_us"],o["p99_us"],o["max_us"],o["lock_wait_total_us"],o["service_total_us"],o["commit_total_us"]);
        }
        println!("最慢请求（本次挂载前 20 项）：");
        for e in value["slowest"].as_array().ok_or("指标格式无效")? {
            println!(
                "{} {} us PID={} {}",
                e["operation"].as_str().unwrap_or(""),
                e["timing"]["total_us"],
                e["pid"],
                e["path_display"].as_str().unwrap_or("—")
            );
        }
    }
    Ok(())
}
