//! Immutable deleted files and snapshot comparison; no FUSE reads or model mutation.
use crate::model::{Attr, Kind, MemFs, SavedNode, Tree, ROOT};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;

pub const TRASH_LIMIT: usize = 1024;
pub const TRASH_CAPACITY: usize = 64 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct TrashRecord {
    pub id: u64,
    pub path: Vec<u8>,
    pub deleted: i64,
    pub reason: String,
    pub attr: Attr,
    pub data: crate::content::Content,
}
impl TrashRecord {
    pub fn saved(&self) -> SavedNode {
        SavedNode {
            attr: self.attr.clone(),
            parent: ROOT,
            name: self.path.rsplit(|b| *b == b'/').next().unwrap().to_vec(),
            data: self.data.clone(),
        }
    }
    pub fn validate(&self, next_id: u64) -> Result<(), String> {
        if self.id == 0
            || self.id >= next_id
            || next_id > i64::MAX as u64
            || !matches!(self.reason.as_str(), "unlink" | "rename_replace")
            || self.attr.kind == Kind::Directory
            || self.attr.mode > 0o777
            || self.attr.nlink != 1
            || self.attr.ino <= ROOT
            || self.attr.ino >= crate::model::VIRTUAL_BASE
            || self.attr.size != self.data.len() as u64
            || self.data.len() > 64 * 1024 * 1024
        {
            return Err("invalid trash record".into());
        }
        let parts: Vec<_> = self.path.split(|b| *b == b'/').collect();
        if parts[0] == b".teamfs" {
            return Err("reserved trash path".into());
        }
        for part in parts {
            MemFs::valid_name(OsStr::from_bytes(part)).map_err(|_| "invalid trash path")?;
        }
        Ok(())
    }
    pub fn metadata(&self) -> Value {
        json!({"id":self.id, "path_bytes":self.path, "path_display":display_path(&self.path),
            "deleted_unix":self.deleted, "reason":self.reason, "size":self.data.len(), "attributes":self.attr})
    }
}

/// Preserve readable Unicode, but escape backslashes, controls and invalid UTF-8 distinctly.
pub fn display_path(bytes: &[u8]) -> String {
    fn append(out: &mut String, text: &str) {
        for c in text.chars() {
            if c == '\\' {
                out.push_str("\\\\");
            } else if c.is_control() {
                out.extend(c.escape_default());
            } else {
                out.push(c);
            }
        }
    }
    let mut out = String::new();
    let mut rest = bytes;
    while !rest.is_empty() {
        match std::str::from_utf8(rest) {
            Ok(s) => {
                append(&mut out, s);
                break;
            }
            Err(e) => {
                append(
                    &mut out,
                    std::str::from_utf8(&rest[..e.valid_up_to()]).unwrap(),
                );
                rest = &rest[e.valid_up_to()..];
                let count = e.error_len().unwrap_or(rest.len());
                for b in &rest[..count] {
                    out.push_str(&format!("\\x{b:02x}"));
                }
                rest = &rest[count..];
            }
        }
    }
    out
}

fn paths(tree: &Tree) -> BTreeMap<Vec<u8>, &SavedNode> {
    let nodes: HashMap<_, _> = tree.nodes.iter().map(|n| (n.attr.ino, n)).collect();
    tree.nodes
        .iter()
        .filter(|n| n.attr.ino != ROOT)
        .map(|n| {
            let mut parts = Vec::new();
            let mut node = n;
            while node.attr.ino != ROOT {
                parts.push(node.name.as_slice());
                node = nodes[&node.parent];
            }
            parts.reverse();
            (parts.join(&b'/'), n)
        })
        .collect()
}

pub fn diff(snapshot: &str, before: &Tree, after: &Tree) -> Result<Value, i32> {
    let old = paths(before);
    let new = paths(after);
    let all: BTreeSet<_> = old.keys().chain(new.keys()).collect();
    let mut changes = Vec::new();
    let mut counts = BTreeMap::from([
        ("added", 0),
        ("removed", 0),
        ("modified", 0),
        ("type_changed", 0),
    ]);
    for path in all {
        let a = old.get(path);
        let b = new.get(path);
        let mut fields = Vec::new();
        let mut content = false;
        let change = match (a, b) {
            (None, Some(_)) => "added",
            (Some(_), None) => "removed",
            (Some(a), Some(b)) => {
                if a.attr.mode != b.attr.mode {
                    fields.push("mode");
                }
                if a.attr.uid != b.attr.uid {
                    fields.push("uid");
                }
                if a.attr.gid != b.attr.gid {
                    fields.push("gid");
                }
                if a.attr.kind != b.attr.kind {
                    "type_changed"
                } else {
                    if a.attr.kind != Kind::Directory {
                        content = !a.data.equals(&b.data)?;
                        if a.attr.mtime != b.attr.mtime {
                            fields.push("mtime");
                        }
                    }
                    if !content && fields.is_empty() {
                        continue;
                    }
                    "modified"
                }
            }
            _ => unreachable!(),
        };
        *counts.get_mut(change).unwrap() += 1;
        changes.push(
            json!({"path_bytes":path, "path_display":display_path(path), "change":change,
            "content_changed":content, "metadata_changed":fields,
            "before":a.map(|n| &n.attr), "after":b.map(|n| &n.attr)}),
        );
    }
    Ok(
        json!({"schema_version":1,"snapshot":snapshot,"captured_unix":time::get_time().sec,
        "summary":counts,"changes":changes}),
    )
}

pub fn json_bytes(value: &Value) -> Vec<u8> {
    let mut bytes = serde_json::to_vec_pretty(value).unwrap();
    bytes.push(b'\n');
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn raw_paths_are_readable_and_unambiguous() {
        assert_eq!(display_path("资料\n".as_bytes()), "资料\\n");
        assert_ne!(display_path(b"a\xff"), display_path(br"a\xff"));
        assert_eq!(display_path(b"a\xff"), "a\\xff");
    }
    #[test]
    fn comparison_ignores_access_time_and_directory_mtime() {
        let before = MemFs::new(1000, 1000).export();
        let mut after = before.clone();
        for n in &mut after.nodes {
            n.attr.atime.sec += 1;
            n.attr.ctime.sec += 1;
            if n.attr.kind == Kind::Directory {
                n.attr.mtime.sec += 1;
            }
        }
        assert!(diff("base", &before, &after).unwrap()["changes"]
            .as_array()
            .unwrap()
            .is_empty());
        let n = after
            .nodes
            .iter_mut()
            .find(|n| n.attr.kind == Kind::File)
            .unwrap();
        let mut bytes = n.data.all().unwrap();
        bytes[0] ^= 1;
        n.data = bytes.into();
        n.attr.mode = 0o600;
        let value = diff("base", &before, &after).unwrap();
        assert_eq!(value["changes"][0]["content_changed"], true);
        assert_eq!(value["changes"][0]["metadata_changed"], json!(["mode"]));
    }
    #[test]
    fn comparison_detects_owner_and_directory_modes_without_contents() {
        let before = MemFs::new(1000, 1000).export();
        let mut after = before.clone();
        let n = after
            .nodes
            .iter_mut()
            .find(|n| n.attr.kind == Kind::Directory && n.attr.ino != ROOT)
            .unwrap();
        n.attr.uid = 2000;
        n.attr.gid = 2001;
        n.attr.mode = 0o700;
        let value = diff("base", &before, &after).unwrap();
        assert_eq!(
            value["changes"][0]["metadata_changed"],
            json!(["mode", "uid", "gid"])
        );
        assert_eq!(value["changes"][0]["content_changed"], false);
    }
}
