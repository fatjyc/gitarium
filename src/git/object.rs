/**
 * [INPUT]: 依赖 store::GitObject 与 GitKind 读取对象内容
 * [OUTPUT]: 导出提交、树、标签的解析函数与 reachable 可达性遍历
 * [POS]: 对象图语义层；拉取打包与推送校验据此决定要遍历的边
 * [PROTOCOL]: 变更时更新此头部，然后检查 CLAUDE.md
 */
use std::collections::HashSet;

use crate::store::{GitKind, GitObject};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TreeEntry {
    pub oid: String,
    pub mode: u32,
    pub name: Vec<u8>,
}

fn header_value<'a>(data: &'a [u8], key: &[u8]) -> Option<&'a [u8]> {
    for line in data.split(|byte| *byte == b'\n') {
        if line.is_empty() {
            break;
        }
        if let Some(rest) = line.strip_prefix(key) {
            return Some(rest);
        }
    }
    None
}

fn header_values<'a>(data: &'a [u8], key: &[u8]) -> Vec<&'a [u8]> {
    let mut values = Vec::new();
    for line in data.split(|byte| *byte == b'\n') {
        if line.is_empty() {
            break;
        }
        if let Some(rest) = line.strip_prefix(key) {
            values.push(rest);
        }
    }
    values
}

fn as_hex_oid(value: &[u8]) -> Result<String, String> {
    if value.len() != 40 || !value.iter().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("对象 ID 不是 40 位十六进制".to_string());
    }
    Ok(String::from_utf8_lossy(value).to_ascii_lowercase())
}

pub fn parse_commit_tree(data: &[u8]) -> Result<String, String> {
    let value = header_value(data, b"tree ").ok_or_else(|| "提交缺少 tree 头".to_string())?;
    as_hex_oid(value)
}

pub fn parse_commit_parents(data: &[u8]) -> Result<Vec<String>, String> {
    header_values(data, b"parent ")
        .into_iter()
        .map(as_hex_oid)
        .collect()
}

pub fn tree_entries(data: &[u8]) -> Result<Vec<TreeEntry>, String> {
    let mut entries = Vec::new();
    let mut position = 0usize;
    while position < data.len() {
        let space = data[position..]
            .iter()
            .position(|byte| *byte == b' ')
            .map(|offset| position + offset)
            .ok_or_else(|| "树条目缺少模式分隔符".to_string())?;
        let mode_text = std::str::from_utf8(&data[position..space])
            .map_err(|_| "树条目的模式不是 ASCII".to_string())?;
        let mode = u32::from_str_radix(mode_text, 8)
            .map_err(|_| format!("树条目的模式无效: {mode_text}"))?;
        let name_start = space + 1;
        let nul = data[name_start..]
            .iter()
            .position(|byte| *byte == 0)
            .map(|offset| name_start + offset)
            .ok_or_else(|| "树条目缺少名称终止符".to_string())?;
        let oid_start = nul + 1;
        let oid_end = oid_start + 20;
        if oid_end > data.len() {
            return Err("树条目的对象 ID 被截断".to_string());
        }
        let mut oid = String::with_capacity(40);
        for byte in &data[oid_start..oid_end] {
            oid.push_str(&format!("{byte:02x}"));
        }
        entries.push(TreeEntry {
            oid,
            mode,
            name: data[name_start..nul].to_vec(),
        });
        position = oid_end;
    }
    Ok(entries)
}

pub fn parse_tag_target(data: &[u8]) -> Result<(String, GitKind), String> {
    let object = header_value(data, b"object ").ok_or_else(|| "标签缺少 object 头".to_string())?;
    let target = as_hex_oid(object)?;
    let kind_text = header_value(data, b"type ").ok_or_else(|| "标签缺少 type 头".to_string())?;
    let kind = match kind_text {
        b"commit" => GitKind::Commit,
        b"tree" => GitKind::Tree,
        b"blob" => GitKind::Blob,
        b"tag" => GitKind::Tag,
        other => {
            return Err(format!(
                "标签目标类型未知: {}",
                String::from_utf8_lossy(other)
            ))
        }
    };
    Ok((target, kind))
}

pub fn reachable<F>(wants: &[String], mut get: F) -> Result<Vec<(String, GitKind)>, String>
where
    F: FnMut(&str) -> Result<Option<GitObject>, String>,
{
    const GITLINK: u32 = 0o160000;
    let mut stack: Vec<String> = wants.to_vec();
    let mut seen: HashSet<String> = HashSet::new();
    let mut found: Vec<(String, GitKind)> = Vec::new();
    while let Some(oid) = stack.pop() {
        if !seen.insert(oid.clone()) {
            continue;
        }
        let object = get(&oid)?.ok_or_else(|| format!("缺少对象 {oid}"))?;
        match object.kind {
            GitKind::Commit => {
                stack.push(parse_commit_tree(&object.data)?);
                stack.extend(parse_commit_parents(&object.data)?);
            }
            GitKind::Tree => {
                for entry in tree_entries(&object.data)? {
                    if entry.mode != GITLINK {
                        stack.push(entry.oid);
                    }
                }
            }
            GitKind::Tag => {
                stack.push(parse_tag_target(&object.data)?.0);
            }
            GitKind::Blob => {}
        }
        found.push((oid, object.kind));
    }
    found.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    const C0: &str = "1111111111111111111111111111111111111111";
    const C1: &str = "2222222222222222222222222222222222222222";
    const T0: &str = "3333333333333333333333333333333333333333";
    const T1: &str = "4444444444444444444444444444444444444444";
    const T2: &str = "5555555555555555555555555555555555555555";
    const B1: &str = "6666666666666666666666666666666666666666";
    const B2: &str = "7777777777777777777777777777777777777777";

    fn oid_bytes(hex: &str) -> Vec<u8> {
        (0..20)
            .map(|i| u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).unwrap())
            .collect()
    }

    fn tree_line(mode: &str, name: &str, oid: &str) -> Vec<u8> {
        let mut data = format!("{mode} {name}\0").into_bytes();
        data.extend_from_slice(&oid_bytes(oid));
        data
    }

    #[test]
    fn commit_fields_are_parsed() {
        let commit = format!("tree {T1}\nparent {C0}\nparent {B1}\nauthor A <a@b> 1 +0000\ncommitter A <a@b> 1 +0000\n\nmsg\n");
        assert_eq!(parse_commit_tree(commit.as_bytes()).unwrap(), T1);
        assert_eq!(
            parse_commit_parents(commit.as_bytes()).unwrap(),
            vec![C0.to_string(), B1.to_string()]
        );
        let root = format!("tree {T0}\n\ntext");
        assert!(parse_commit_parents(root.as_bytes()).unwrap().is_empty());
    }

    #[test]
    fn tree_entries_include_modes_and_binary_names() {
        let mut data = tree_line("100644", "a.txt", B1);
        data.extend(tree_line("40000", "dir", T2));
        let entries = tree_entries(&data).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].mode, 0o100644);
        assert_eq!(entries[0].name, b"a.txt".to_vec());
        assert_eq!(entries[0].oid, B1);
        assert_eq!(entries[1].mode, 0o40000);
        assert_eq!(entries[1].oid, T2);
    }

    #[test]
    fn annotated_tag_reports_target_and_kind() {
        let tag = format!("object {C0}\ntype commit\ntag v1\ntagger A <a@b> 1 +0000\n\nrelease\n");
        assert_eq!(
            parse_tag_target(tag.as_bytes()).unwrap(),
            (C0.to_string(), GitKind::Commit)
        );
    }

    #[test]
    fn reachable_walks_commits_trees_and_tags_but_skips_gitlinks() {
        let mut objects: HashMap<String, GitObject> = HashMap::new();
        let mut put = |oid: &str, kind: GitKind, data: Vec<u8>| {
            objects.insert(oid.to_string(), GitObject { kind, data });
        };
        put(
            C1,
            GitKind::Commit,
            format!("tree {T1}\nparent {C0}\n\nmsg").into_bytes(),
        );
        put(
            C0,
            GitKind::Commit,
            format!("tree {T0}\n\nroot").into_bytes(),
        );
        put(T1, GitKind::Tree, {
            let mut data = tree_line("100644", "a.txt", B1);
            data.extend(tree_line("40000", "dir", T2));
            data
        });
        put(T0, GitKind::Tree, tree_line("100644", "a.txt", B1));
        put(T2, GitKind::Tree, {
            let mut data = tree_line("100644", "b.txt", B2);
            data.extend(tree_line(
                "160000",
                "sub",
                "9999999999999999999999999999999999999999",
            ));
            data
        });
        put(B1, GitKind::Blob, b"one".to_vec());
        put(B2, GitKind::Blob, b"two".to_vec());

        let found = reachable(&[C1.to_string()], |oid| Ok(objects.get(oid).cloned())).unwrap();
        let ids: Vec<&str> = found.iter().map(|(oid, _)| oid.as_str()).collect();
        assert_eq!(ids, vec![C0, C1, T0, T1, T2, B1, B2]);
        assert_eq!(
            found
                .iter()
                .find(|(oid, _)| oid == T2)
                .map(|(_, kind)| *kind),
            Some(GitKind::Tree)
        );
        assert!(found
            .iter()
            .all(|(oid, _)| oid != "9999999999999999999999999999999999999999"));
    }

    #[test]
    fn reachable_reports_missing_objects() {
        let error = reachable(&[C1.to_string()], |_| Ok(None)).unwrap_err();
        assert!(error.contains(C1), "{error}");
    }
}
