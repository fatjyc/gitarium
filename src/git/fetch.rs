/**
 * [INPUT]: 依赖 store::Store 读取对象，依赖 object::reachable 计算对象集合，依赖 pack::encode 与 wire 写出
 * [OUTPUT]: 导出 upload，处理 git-upload-pack 请求并返回 NAK 与原始 pack
 * [POS]: 拉取语义的唯一实现；忽略 have 协商，始终发送 wants 可达对象，牺牲流量换取确定性
 * [PROTOCOL]: 变更时更新此头部，然后检查 CLAUDE.md
 */
use std::io::{BufRead, Write};

use crate::git::object::reachable;
use crate::git::pack;
use crate::git::wire::{read_pkt, write_pkt, Pkt};
use crate::store::{validate_repo_name, Store};

fn is_hex_oid(value: &str) -> bool {
    value.len() == 40 && value.chars().all(|c| c.is_ascii_hexdigit())
}

fn collect_wants(line: &[u8]) -> Result<Option<String>, String> {
    let text = std::str::from_utf8(line).map_err(|_| "拉取请求不是 UTF-8".to_string())?;
    let text = text.trim_end_matches('\n');
    if let Some(rest) = text.strip_prefix("want ") {
        let oid = rest.split(' ').next().unwrap_or("");
        if !is_hex_oid(oid) {
            return Err(format!("want 的对象 ID 无效: {oid}"));
        }
        return Ok(Some(oid.to_ascii_lowercase()));
    }
    Ok(None)
}

pub fn upload(
    store: &Store,
    repo: &str,
    input: &mut dyn BufRead,
    out: &mut dyn Write,
    max: usize,
) -> Result<(), String> {
    validate_repo_name(repo)?;
    let mut wants: Vec<String> = Vec::new();
    loop {
        match read_pkt(input)? {
            Pkt::Flush => continue,
            Pkt::Eof => break,
            Pkt::Line(line) => {
                let text = String::from_utf8_lossy(&line);
                let trimmed = text.trim_end_matches('\n');
                if trimmed == "done" {
                    break;
                }
                if let Some(oid) = collect_wants(&line)? {
                    if !wants.contains(&oid) {
                        wants.push(oid);
                    }
                }
            }
        }
    }
    write_pkt(out, b"NAK\n")?;
    let mut objects = Vec::new();
    if !wants.is_empty() {
        let reachable = reachable(&wants, |oid| store.get_object(repo, oid))?;
        let mut total = 0usize;
        for (oid, kind) in reachable {
            let object = store
                .get_object(repo, &oid)?
                .ok_or_else(|| format!("缺少对象 {oid}"))?;
            total = total.saturating_add(object.data.len());
            objects.push((oid, kind, object.data));
        }
        if total > max.saturating_mul(4) {
            return Err(format!("拉取对象总大小 {total} 超过上限"));
        }
    }
    pack::encode(&objects, out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn want_lines_extract_oid_and_ignore_capabilities() {
        let oid = "1".repeat(40);
        let line = format!("want {oid} multi_ack side-band-64k\n");
        assert_eq!(
            collect_wants(line.as_bytes()).unwrap().as_deref(),
            Some(oid.as_str())
        );
    }

    #[test]
    fn have_lines_are_not_wants() {
        let oid = "1".repeat(40);
        assert_eq!(
            collect_wants(format!("have {oid}\n").as_bytes()).unwrap(),
            None
        );
    }

    #[test]
    fn malformed_want_is_rejected() {
        assert!(collect_wants(b"want not-an-oid\n").is_err());
    }
}
