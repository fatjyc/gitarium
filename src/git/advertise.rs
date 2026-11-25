/**
 * [INPUT]: 依赖 store::Store 读取引用与对象，依赖 wire 生成 pkt-line
 * [OUTPUT]: 导出 Service 与 refs 引用通告
 * [POS]: 拉取与推送共用的引用广播；HEAD 符号引用、能力行与标签 peel 在这里决定
 * [PROTOCOL]: 变更时更新此头部，然后检查 CLAUDE.md
 */
use crate::git::object::parse_tag_target;
use crate::git::wire::{write_flush, write_pkt};
use crate::store::{GitKind, Store};

const AGENT: &str = "agent=gitarium/0.1";
const ZERO_OID: &str = "0000000000000000000000000000000000000000";
const HEAD_REF: &str = "refs/heads/main";

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Service {
    UploadPack,
    ReceivePack,
}

impl Service {
    pub fn name(self) -> &'static str {
        match self {
            Service::UploadPack => "git-upload-pack",
            Service::ReceivePack => "git-receive-pack",
        }
    }

    pub fn is_receive(self) -> bool {
        matches!(self, Service::ReceivePack)
    }
}

fn capabilities(service: Service) -> String {
    match service {
        Service::UploadPack => AGENT.to_string(),
        Service::ReceivePack => format!("report-status delete-refs {AGENT}"),
    }
}

fn peel(store: &Store, repo: &str, oid: &str) -> Result<String, String> {
    let mut current = oid.to_string();
    for _ in 0..16 {
        match store.get_object(repo, &current)? {
            Some(object) if object.kind == GitKind::Tag => {
                current = parse_tag_target(&object.data)?.0;
            }
            _ => return Ok(current),
        }
    }
    Err("标签链过长".to_string())
}

pub fn refs(store: &Store, repo: &str, service: Service) -> Result<Vec<u8>, String> {
    let caps = capabilities(service);
    let references = store.list_refs(repo)?;
    let head_oid = references
        .iter()
        .find(|(name, _)| name == HEAD_REF)
        .map(|(_, oid)| oid.clone());

    let mut out = Vec::new();
    let mut first = true;
    if let Some(oid) = head_oid {
        let line = if service == Service::UploadPack {
            format!("{oid} HEAD\0symref=HEAD:{HEAD_REF} {caps}\n")
        } else {
            format!("{oid} HEAD\0{caps}\n")
        };
        write_pkt(&mut out, line.as_bytes())?;
        first = false;
    } else if references.is_empty() {
        write_pkt(
            &mut out,
            format!("{ZERO_OID} capabilities^{{}}\0{caps}\n").as_bytes(),
        )?;
        write_flush(&mut out)?;
        return Ok(out);
    }

    for (name, oid) in &references {
        if first {
            write_pkt(&mut out, format!("{oid} {name}\0{caps}\n").as_bytes())?;
            first = false;
        } else {
            write_pkt(&mut out, format!("{oid} {name}\n").as_bytes())?;
        }
        if name.starts_with("refs/tags/") {
            let peeled = peel(store, repo, oid)?;
            if &peeled != oid {
                write_pkt(&mut out, format!("{peeled} {name}^{{}}\n").as_bytes())?;
            }
        }
    }
    write_flush(&mut out)?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::wire::{read_pkt, Pkt};
    use crate::store::{GitKind, Store};

    const ZERO: &str = "0000000000000000000000000000000000000000";
    const COMMIT: &str = "1111111111111111111111111111111111111111";

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
                Pkt::Eof => panic!("通告缺少 flush"),
            }
        }
        result
    }

    #[test]
    fn empty_repository_advertises_capabilities_line() {
        let (store, _dir) = store();
        let advertisement = refs(&store, "missing", Service::UploadPack).unwrap();
        let lines = lines(&advertisement);
        assert_eq!(lines.len(), 1);
        assert!(
            lines[0].starts_with(&format!("{ZERO} capabilities^{{}}\0")),
            "{}",
            lines[0]
        );
        assert!(lines[0].contains("agent=gitarium/"));
    }

    #[test]
    fn head_is_first_and_carries_symref() {
        let (store, _dir) = store();
        store.ensure_repo("demo").unwrap();
        store
            .update_ref("demo", "refs/heads/main", None, Some(COMMIT))
            .unwrap();
        let lines = lines(&refs(&store, "demo", Service::UploadPack).unwrap());
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert!(
            lines[0].starts_with(&format!("{COMMIT} HEAD\0")),
            "{}",
            lines[0]
        );
        assert!(
            lines[0].contains("symref=HEAD:refs/heads/main"),
            "{}",
            lines[0]
        );
        assert_eq!(lines[1], format!("{COMMIT} refs/heads/main\n"));
    }

    #[test]
    fn annotated_tag_is_peeled() {
        let (store, _dir) = store();
        store.ensure_repo("demo").unwrap();
        store
            .update_ref("demo", "refs/heads/main", None, Some(COMMIT))
            .unwrap();
        let tag =
            format!("object {COMMIT}\ntype commit\ntag v1\ntagger A <a@b> 1 +0000\n\nrelease\n");
        let tag_oid = crate::git::oid::object_oid(GitKind::Tag, tag.as_bytes());
        store
            .put_object("demo", &tag_oid, GitKind::Tag, tag.as_bytes())
            .unwrap();
        store
            .update_ref("demo", "refs/tags/v1", None, Some(&tag_oid))
            .unwrap();

        let lines = lines(&refs(&store, "demo", Service::UploadPack).unwrap());
        assert!(
            lines
                .iter()
                .any(|line| line == &format!("{tag_oid} refs/tags/v1\n")),
            "{lines:?}"
        );
        assert!(
            lines
                .iter()
                .any(|line| line == &format!("{COMMIT} refs/tags/v1^{{}}\n")),
            "{lines:?}"
        );
    }

    #[test]
    fn receive_pack_advertises_report_status_and_delete_refs() {
        let (store, _dir) = store();
        let lines = lines(&refs(&store, "missing", Service::ReceivePack).unwrap());
        assert!(lines[0].contains("report-status"), "{}", lines[0]);
        assert!(lines[0].contains("delete-refs"), "{}", lines[0]);
        assert!(
            !lines[0].contains("symref="),
            "接收端不需要 symref: {}",
            lines[0]
        );
    }
}
