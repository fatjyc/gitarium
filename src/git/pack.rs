/**
 * [INPUT]: 依赖 gix-pack 解码包、flate2 解压与压缩、sha1 计算包尾；依赖 git::oid、git::delta、store::GitObject
 * [OUTPUT]: 导出 Ingested、decode 与 encode
 * [POS]: 传输包与对象模型之间的唯一转换点；解码支持薄包，编码不产生差量
 * [PROTOCOL]: 变更时更新此头部，然后检查 CLAUDE.md
 */
use std::collections::{BTreeMap, HashMap};
use std::io::{BufRead, Read, Write};

use flate2::read::ZlibDecoder;
use flate2::write::ZlibEncoder;
use flate2::Compression;
use gix_pack::data::entry::Header;
use gix_pack::data::input::{BytesToEntriesIter, EntryDataMode, Mode};
use sha1::{Digest, Sha1};

use crate::git::delta::apply_delta;
use crate::git::oid::object_oid;
use crate::store::{GitKind, GitObject};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ingested {
    pub objects: Vec<(String, GitKind, Vec<u8>)>,
}

struct PendingDelta {
    pack_offset: u64,
    header: Header,
    compressed: Vec<u8>,
}

fn kind_of(header: &Header) -> Option<GitKind> {
    match header {
        Header::Commit => Some(GitKind::Commit),
        Header::Tree => Some(GitKind::Tree),
        Header::Blob => Some(GitKind::Blob),
        Header::Tag => Some(GitKind::Tag),
        _ => None,
    }
}

pub fn decode(
    input: &mut dyn BufRead,
    lookup: &dyn Fn(&str) -> Result<Option<GitObject>, String>,
    limit: usize,
) -> Result<Ingested, String> {
    let iterator = BytesToEntriesIter::new_from_header(
        input,
        Mode::Verify,
        EntryDataMode::Keep,
        gix_hash::Kind::Sha1,
    )
    .map_err(|e| format!("读取包失败: {e}"))?;

    let mut objects: BTreeMap<String, (GitKind, Vec<u8>)> = BTreeMap::new();
    let mut by_offset: HashMap<u64, (GitKind, Vec<u8>)> = HashMap::new();
    let mut deltas: Vec<PendingDelta> = Vec::new();
    let mut total = 0usize;

    for entry in iterator {
        let entry = entry.map_err(|e| format!("解码包失败: {e}"))?;
        total = total.saturating_add(entry.decompressed_size as usize);
        if total > limit {
            return Err(format!("推送包解压后超过上限 {limit} 字节"));
        }
        let compressed = entry
            .compressed
            .ok_or_else(|| "包条目缺少压缩数据".to_string())?;
        match kind_of(&entry.header) {
            Some(kind) => {
                let data = inflate(&compressed)?;
                let oid = object_oid(kind, &data);
                by_offset.insert(entry.pack_offset, (kind, data.clone()));
                objects.entry(oid).or_insert((kind, data));
            }
            None => deltas.push(PendingDelta {
                pack_offset: entry.pack_offset,
                header: entry.header,
                compressed,
            }),
        }
    }

    while !deltas.is_empty() {
        let mut remaining = Vec::new();
        let mut progress = false;
        for delta in deltas.drain(..) {
            let base = match &delta.header {
                Header::OfsDelta { base_distance } => {
                    let base_offset = delta
                        .pack_offset
                        .checked_sub(*base_distance)
                        .ok_or_else(|| "差量基对象偏移无效".to_string())?;
                    by_offset.get(&base_offset).cloned()
                }
                Header::RefDelta { base_id } => {
                    let id = base_id.to_hex().to_string();
                    match objects.get(&id) {
                        Some((kind, data)) => Some((*kind, data.clone())),
                        None => lookup(&id)?.map(|object| (object.kind, object.data)),
                    }
                }
                _ => unreachable!("kind_of 已区分非差量条目"),
            };
            match base {
                Some((kind, base_data)) => {
                    let delta_data = inflate(&delta.compressed)?;
                    let data = apply_delta(&base_data, &delta_data)?;
                    let oid = object_oid(kind, &data);
                    by_offset.insert(delta.pack_offset, (kind, data.clone()));
                    objects.entry(oid).or_insert((kind, data));
                    progress = true;
                }
                None => remaining.push(delta),
            }
        }
        deltas = remaining;
        if !progress && !deltas.is_empty() {
            return Err(format!("有 {} 个差量缺少基对象", deltas.len()));
        }
    }

    Ok(Ingested {
        objects: objects
            .into_iter()
            .map(|(oid, (kind, data))| (oid, kind, data))
            .collect(),
    })
}

pub fn encode(objects: &[(String, GitKind, Vec<u8>)], out: &mut dyn Write) -> Result<(), String> {
    let count = u32::try_from(objects.len()).map_err(|_| "对象数量超过 u32".to_string())?;
    let mut writer = HashingWriter {
        out,
        hasher: Sha1::new(),
    };
    writer.write_all(b"PACK").map_err(write_error)?;
    writer.write_all(&2u32.to_be_bytes()).map_err(write_error)?;
    writer
        .write_all(&count.to_be_bytes())
        .map_err(write_error)?;

    for (_, kind, data) in objects {
        write_entry_header(&mut writer, *kind, data.len())?;
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder
            .write_all(data)
            .map_err(|e| format!("压缩对象失败: {e}"))?;
        let compressed = encoder.finish().map_err(|e| format!("压缩对象失败: {e}"))?;
        writer.write_all(&compressed).map_err(write_error)?;
    }
    let trailer = writer.hasher.clone().finalize();
    writer.out.write_all(&trailer).map_err(write_error)?;
    Ok(())
}

fn write_entry_header(out: &mut dyn Write, kind: GitKind, size: usize) -> Result<(), String> {
    let mut byte = ((kind as u8) << 4) | ((size & 0x0f) as u8);
    let mut remaining = size >> 4;
    while remaining > 0 {
        out.write_all(&[byte | 0x80]).map_err(write_error)?;
        byte = (remaining & 0x7f) as u8;
        remaining >>= 7;
    }
    out.write_all(&[byte]).map_err(write_error)
}

fn write_error(error: std::io::Error) -> String {
    format!("写包失败: {error}")
}

struct HashingWriter<'a> {
    out: &'a mut dyn Write,
    hasher: Sha1,
}

impl Write for HashingWriter<'_> {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        self.hasher.update(buffer);
        self.out.write_all(buffer)?;
        Ok(buffer.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.out.flush()
    }
}

fn inflate(compressed: &[u8]) -> Result<Vec<u8>, String> {
    let mut decoder = ZlibDecoder::new(compressed);
    let mut data = Vec::new();
    decoder
        .read_to_end(&mut data)
        .map_err(|e| format!("解压对象失败: {e}"))?;
    Ok(data)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::process::Command;

    struct Fixture {
        _dir: tempfile::TempDir,
        path: std::path::PathBuf,
    }

    impl Fixture {
        fn new() -> Fixture {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("repo");
            run(
                dir.path(),
                &["init", "-q", "-b", "main", path.to_str().unwrap()],
            );
            run(&path, &["config", "user.name", "Test"]);
            run(&path, &["config", "user.email", "test@example.com"]);
            Fixture { _dir: dir, path }
        }

        fn commit(&self, file: &str, content: &[u8], message: &str) -> String {
            std::fs::write(self.path.join(file), content).unwrap();
            run(&self.path, &["add", file]);
            run(&self.path, &["commit", "-q", "-m", message]);
            self.rev("HEAD")
        }

        fn rev(&self, name: &str) -> String {
            let output = Command::new("git")
                .args(["rev-parse", name])
                .current_dir(&self.path)
                .output()
                .unwrap();
            assert!(output.status.success());
            String::from_utf8(output.stdout).unwrap().trim().to_string()
        }

        fn pack(&self, args: &[&str], stdin: &[u8]) -> Vec<u8> {
            use std::io::Write as _;
            let mut child = Command::new("git")
                .arg("pack-objects")
                .args(args)
                .current_dir(&self.path)
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .spawn()
                .unwrap();
            child.stdin.as_mut().unwrap().write_all(stdin).unwrap();
            let output = child.wait_with_output().unwrap();
            assert!(output.status.success(), "git pack-objects 失败");
            output.stdout
        }

        fn all_objects(&self) -> BTreeMap<String, String> {
            let output = Command::new("git")
                .args([
                    "cat-file",
                    "--batch-all-objects",
                    "--batch-check=%(objectname) %(objecttype)",
                ])
                .current_dir(&self.path)
                .output()
                .unwrap();
            assert!(output.status.success());
            String::from_utf8(output.stdout)
                .unwrap()
                .lines()
                .map(|line| {
                    let (oid, kind) = line.split_once(' ').unwrap();
                    (oid.to_string(), kind.to_string())
                })
                .collect()
        }
    }

    fn run(cwd: &std::path::Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {:?} 失败: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn kind_name(kind: GitKind) -> &'static str {
        kind.as_str()
    }

    #[test]
    fn decode_reads_full_pack_from_git() {
        let fixture = Fixture::new();
        fixture.commit("a.txt", b"one\ntwo\n", "first");
        fixture.commit("notes.txt", b"hello\n", "second");
        let pack = fixture.pack(&["--stdout", "--all"], b"");
        let decoded = decode(&mut pack.as_slice(), &|_| Ok(None), 64 * 1024 * 1024).unwrap();

        let expected = fixture.all_objects();
        let mut actual: BTreeMap<String, String> = BTreeMap::new();
        for (oid, kind, data) in &decoded.objects {
            assert_eq!(&object_oid(*kind, data), oid, "对象 ID 应由内容计算得到");
            actual.insert(oid.clone(), kind_name(*kind).to_string());
        }
        assert_eq!(actual, expected);
    }

    #[test]
    fn decode_rejects_pack_over_limit() {
        let fixture = Fixture::new();
        fixture.commit("a.txt", &vec![b'x'; 4096], "big");
        let pack = fixture.pack(&["--stdout", "--all"], b"");
        let error = decode(&mut pack.as_slice(), &|_| Ok(None), 1024).unwrap_err();
        assert!(error.contains("上限") || error.contains("limit"), "{error}");
    }

    #[test]
    fn decode_resolves_thin_pack_base_from_lookup() {
        let fixture = Fixture::new();
        let old = fixture.commit("a.txt", b"line one\nline two\n", "first");
        let full = fixture.pack(&["--stdout", "--all"], b"");
        let old_objects = decode(&mut full.as_slice(), &|_| Ok(None), 64 * 1024 * 1024).unwrap();
        let new = fixture.commit("a.txt", b"line one\nline two changed\n", "second");

        let request = format!("{new}\n--not\n{old}\n");
        let thin = fixture.pack(&["--thin", "--stdout", "--revs"], request.as_bytes());
        let lookup = |oid: &str| {
            Ok(old_objects
                .objects
                .iter()
                .find(|(candidate, _, _)| candidate == oid)
                .map(|(_, kind, data)| GitObject {
                    kind: *kind,
                    data: data.clone(),
                }))
        };
        let decoded = decode(&mut thin.as_slice(), &lookup, 64 * 1024 * 1024).unwrap();
        assert!(
            decoded.objects.iter().any(|(oid, _, _)| oid == &new),
            "薄包解码后应包含新提交"
        );
        for (oid, kind, data) in &decoded.objects {
            assert_eq!(&object_oid(*kind, data), oid);
        }
    }

    #[test]
    fn encode_writes_pack_indexable_by_git() {
        let fixture = Fixture::new();
        fixture.commit("a.txt", b"alpha\n", "first");
        let pack = fixture.pack(&["--stdout", "--all"], b"");
        let decoded = decode(&mut pack.as_slice(), &|_| Ok(None), 64 * 1024 * 1024).unwrap();

        let mut encoded = Vec::new();
        encode(&decoded.objects, &mut encoded).unwrap();

        let dir = tempfile::tempdir().unwrap();
        run(dir.path(), &["init", "-q", "--bare"]);
        use std::io::Write as _;
        let mut child = Command::new("git")
            .args(["index-pack", "--stdin"])
            .current_dir(dir.path())
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.as_mut().unwrap().write_all(&encoded).unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "git index-pack 拒绝我们编码的包: {}",
            String::from_utf8_lossy(&output.stderr)
        );

        let output = Command::new("git")
            .args([
                "cat-file",
                "--batch-all-objects",
                "--batch-check=%(objectname) %(objecttype)",
            ])
            .current_dir(dir.path())
            .output()
            .unwrap();
        let actual: BTreeMap<String, String> = String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .map(|line| {
                let (oid, kind) = line.split_once(' ').unwrap();
                (oid.to_string(), kind.to_string())
            })
            .collect();
        let expected: BTreeMap<String, String> = decoded
            .objects
            .iter()
            .map(|(oid, kind, _)| (oid.clone(), kind_name(*kind).to_string()))
            .collect();
        assert_eq!(actual, expected);
    }
}
