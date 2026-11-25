/**
 * [INPUT]: 依赖 sha1 计算摘要，依赖 store::GitKind 生成对象头
 * [OUTPUT]: 导出 object_oid 与 object_header
 * [POS]: Git 对象身份的唯一计算处；写入与读取都以这里的 ID 为准
 * [PROTOCOL]: 变更时更新此头部，然后检查 CLAUDE.md
 */
use sha1::{Digest, Sha1};

use crate::store::GitKind;

pub fn object_header(kind: GitKind, len: usize) -> Vec<u8> {
    format!("{} {}\0", kind.as_str(), len).into_bytes()
}

pub fn object_oid(kind: GitKind, data: &[u8]) -> String {
    let mut hasher = Sha1::new();
    hasher.update(object_header(kind, data.len()));
    hasher.update(data);
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(40);
    for byte in digest {
        hex.push_str(&format!("{byte:02x}"));
    }
    hex
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_blob_matches_git_reference() {
        assert_eq!(
            object_oid(GitKind::Blob, b""),
            "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391"
        );
    }

    #[test]
    fn blob_with_binary_and_chinese_bytes_matches_git_reference() {
        let data = b"a\x00b\r\n\xe4\xb8\xad\xe6\x96\x87";
        assert_eq!(
            object_oid(GitKind::Blob, data),
            "14b9b21e757a3aa05978391b376b4128a1cd3202"
        );
    }

    #[test]
    fn text_blob_matches_git_reference() {
        assert_eq!(
            object_oid(GitKind::Blob, b"hello world\n"),
            "3b18e512dba79e4c8300dd08aeb37f8e728b8dad"
        );
    }

    #[test]
    fn header_contains_kind_and_length() {
        assert_eq!(object_header(GitKind::Blob, 12), b"blob 12\0".to_vec());
        assert_eq!(object_header(GitKind::Tree, 0), b"tree 0\0".to_vec());
    }
}
