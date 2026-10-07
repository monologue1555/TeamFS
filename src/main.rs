mod filesystem;
mod model;
mod service;
mod store;
use service::{Command, Service};
use std::{
    cell::RefCell,
    ffi::{OsStr, OsString},
    fs::OpenOptions,
    io::Write,
    os::unix::ffi::OsStrExt,
    path::PathBuf,
    rc::Rc,
};

fn main() {
    if let Err(error) = run() {
        eprintln!("TeamFS: {error}");
        std::process::exit(1);
    }
}
fn help() {
    println!("TeamFS 0.2 — 可保存、可恢复的用户空间文件系统\n\nteamfs mount <挂载点> [--store <目录> | --memory] [--trace]\nteamfs sync <挂载点>\nteamfs snapshot create <挂载点> <名称>\nteamfs snapshot list <挂载点>\nteamfs snapshot delete <挂载点> <名称>\nteamfs restore <挂载点> <快照> <源相对路径> <目标相对路径>\n\n默认持久化；普通修改在 sync/fsync/正常卸载时保存。异常退出只恢复最后的成功同步。\n创建/删除快照会同步当前文件树。restore 不覆盖，恢复后需要同步。\n--memory 是教学内存模式，退出后包括快照在内的所有数据丢失。");
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
    if matches!(command, Command::Restore { .. }) {
        println!("文件已恢复到新路径；尚需 teamfs sync 或正常卸载才能持久保存。");
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
        Some("mount") => mount(&args[1..]),
        Some("sync") if args.len() == 2 => send(&args[1], Command::Sync),
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
fn mount(args: &[OsString]) -> Result<(), String> {
    if args.is_empty() {
        return Err("缺少挂载点".into());
    }
    let mut trace = false;
    let mut memory = false;
    let mut store = None;
    let mut i = 1;
    while i < args.len() {
        match args[i].to_str() {
            Some("--trace") => trace = true,
            Some("--memory") => memory = true,
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
    let service = Rc::new(RefCell::new(Service::new(
        unsafe { libc::geteuid() },
        unsafe { libc::getegid() },
        store.as_deref(),
    )?));
    eprintln!(
        "TeamFS 挂载：{}；模式={}；存储={:?}",
        mountpoint.display(),
        service.borrow().mode(),
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
    let mounted = fuse::mount(
        filesystem::TeamFs::new(service.clone(), trace),
        &mountpoint,
        &options,
    );
    // 不依赖 FUSE destroy 或 Drop：挂载循环结束后显式检查保存结果。
    let saved = service.borrow_mut().sync();
    if saved.is_err() {
        return Err(format!(
            "退出保存失败：{}",
            service
                .borrow()
                .last_sync_error
                .as_deref()
                .unwrap_or("unknown")
        ));
    }
    mounted.map_err(|e| format!("挂载失败：{e}"))?;
    eprintln!("TeamFS 已正常卸载；persistent 模式已同步，memory 模式数据已结束。");
    Ok(())
}
