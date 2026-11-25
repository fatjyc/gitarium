/**
 * [INPUT]: 依赖 store::Store 写对象与比较并交换引用，依赖 git::pack 解码推送包，依赖 git::wire 读写状态
 * [OUTPUT]: 导出 receive，处理 git-receive-pack 请求并写 report-status
 * [POS]: 推送语义的唯一实现；HTTP 与 SSH 传输都调用这里，不直接处理引用
 * [PROTOCOL]: 变更时更新此头部，然后检查 CLAUDE.md
 */
use std::io::{BufRead, Write};

use crate::git::pack;
use crate::git::wire::{read_pkt, write_flush, write_pkt, Pkt};
use crate::store::{validate_ref_name, validate_repo_name, Store};

const ZERO_OID: &str = "0000000000000000000000000000000000000000";

struct Command {
    old: String,
    new: String,
    name: String,
}

fn is_hex_oid(value: &str) -> bool {
    value.len() == 40 && value.chars().all(|c| c.is_ascii_hexdigit())
}

fn parse_command(line: &[u8]) -> Result<(Command, Vec<String>), String> {
    let (command_part, capabilities) = match line.iter().position(|byte| *byte == 0) {
        Some(index) => (&line[..index], Some(&line[index + 1..])),
        None => (line, None),
    };
    let text = std::str::from_utf8(command_part).map_err(|_| "推送命令不是 UTF-8".to_string())?;
    let mut fields = text.trim_end().split(' ');
    let old = fields
        .next()
        .ok_or_else(|| "推送命令缺少旧值".to_string())?;
    let new = fields
        .next()
        .ok_or_else(|| "推送命令缺少新值".to_string())?;
    let name = fields
        .next()
        .ok_or_else(|| "推送命令缺少引用名".to_string())?;
    if fields.next().is_some() {
        return Err("推送命令字段过多".to_string());
    }
    if !is_hex_oid(old) || !is_hex_oid(new) {
        return Err("推送命令的对象 ID 无效".to_string());
    }
    let caps = capabilities
        .map(|caps| {
            String::from_utf8_lossy(caps)
                .split_whitespace()
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    Ok((
        Command {
            old: old.to_ascii_lowercase(),
            new: new.to_ascii_lowercase(),
            name: name.to_string(),
        },
        caps,
    ))
}

pub fn receive(
    store: &Store,
    repo: &str,
    input: &mut dyn BufRead,
    out: &mut dyn Write,
    max: usize,
) -> Result<(), String> {
    validate_repo_name(repo)?;
    let mut commands = Vec::new();
    let mut capabilities = Vec::new();
    loop {
        match read_pkt(input)? {
            Pkt::Flush => break,
            Pkt::Eof => break,
            Pkt::Line(line) => {
                let (command, caps) = parse_command(&line)?;
                if commands.is_empty() {
                    capabilities = caps;
                }
                commands.push(command);
            }
        }
    }
    let wants_report = capabilities.iter().any(|cap| cap == "report-status");
    if commands.is_empty() {
        return Ok(());
    }

    let needs_pack = commands.iter().any(|command| command.new != ZERO_OID);
    if needs_pack {
        let lookup = |oid: &str| store.get_object(repo, oid);
        let ingested = match pack::decode(input, &lookup, max) {
            Ok(ingested) => ingested,
            Err(error) => {
                if wants_report {
                    write_pkt(out, format!("unpack {error}\n").as_bytes())?;
                    write_flush(out)?;
                }
                return Ok(());
            }
        };
        store.ensure_repo(repo)?;
        for (oid, kind, data) in &ingested.objects {
            store.put_object(repo, oid, *kind, data)?;
        }
    }

    let mut statuses: Vec<(String, Option<String>)> = Vec::new();
    for command in &commands {
        if let Err(error) = validate_ref_name(&command.name) {
            statuses.push((command.name.clone(), Some(error)));
            continue;
        }
        if command.new == ZERO_OID {
            let expected = if command.old == ZERO_OID {
                None
            } else {
                Some(command.old.as_str())
            };
            match store.update_ref(repo, &command.name, expected, None) {
                Ok(true) => statuses.push((command.name.clone(), None)),
                Ok(false) => statuses.push((
                    command.name.clone(),
                    Some("failed to update ref".to_string()),
                )),
                Err(error) => statuses.push((command.name.clone(), Some(error))),
            }
            continue;
        }
        if !store.has_object(repo, &command.new)? {
            statuses.push((
                command.name.clone(),
                Some("missing necessary objects".to_string()),
            ));
            continue;
        }
        let expected = if command.old == ZERO_OID {
            None
        } else {
            Some(command.old.as_str())
        };
        match store.update_ref(repo, &command.name, expected, Some(&command.new)) {
            Ok(true) => statuses.push((command.name.clone(), None)),
            Ok(false) => statuses.push((
                command.name.clone(),
                Some("failed to update ref".to_string()),
            )),
            Err(error) => statuses.push((command.name.clone(), Some(error))),
        }
    }

    if wants_report {
        write_pkt(out, b"unpack ok\n")?;
        for (name, error) in &statuses {
            let line = match error {
                None => format!("ok {name}\n"),
                Some(reason) => format!("ng {name} {reason}\n"),
            };
            write_pkt(out, line.as_bytes())?;
        }
        write_flush(out)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::wire::read_pkt;

    fn store() -> (Store, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("git.db").to_str().unwrap()).unwrap();
        (store, dir)
    }

    fn lines(bytes: &[u8]) -> Vec<String> {
        let mut input = bytes;
        let mut result = Vec::new();
        loop {
            match read_pkt(&mut input).unwrap() {
                Pkt::Flush => break,
                Pkt::Line(line) => result.push(String::from_utf8(line).unwrap()),
                Pkt::Eof => panic!("报告缺少 flush"),
            }
        }
        result
    }

    #[test]
    fn delete_with_stale_old_value_reports_failure_and_keeps_ref() {
        let (store, _dir) = store();
        let stored = "a".repeat(40);
        let stale = "b".repeat(40);
        store.ensure_repo("demo").unwrap();
        store
            .update_ref("demo", "refs/heads/main", None, Some(&stored))
            .unwrap();
        let mut request = Vec::new();
        write_pkt(
            &mut request,
            format!("{stale} {} refs/heads/main\0report-status\n", ZERO_OID).as_bytes(),
        )
        .unwrap();
        write_flush(&mut request).unwrap();

        let mut out = Vec::new();
        receive(&store, "demo", &mut request.as_slice(), &mut out, 1024).unwrap();
        let report = lines(&out);
        assert_eq!(report[0], "unpack ok\n");
        assert_eq!(report[1], "ng refs/heads/main failed to update ref\n");
        assert_eq!(
            store.get_ref("demo", "refs/heads/main").unwrap().as_deref(),
            Some(stored.as_str())
        );
    }

    #[test]
    fn deleting_existing_ref_reports_ok() {
        let (store, _dir) = store();
        let stored = "a".repeat(40);
        store.ensure_repo("demo").unwrap();
        store
            .update_ref("demo", "refs/heads/topic", None, Some(&stored))
            .unwrap();
        let mut request = Vec::new();
        write_pkt(
            &mut request,
            format!("{stored} {} refs/heads/topic\0report-status\n", ZERO_OID).as_bytes(),
        )
        .unwrap();
        write_flush(&mut request).unwrap();

        let mut out = Vec::new();
        receive(&store, "demo", &mut request.as_slice(), &mut out, 1024).unwrap();
        assert_eq!(lines(&out)[1], "ok refs/heads/topic\n");
        assert!(store.get_ref("demo", "refs/heads/topic").unwrap().is_none());
    }
}
