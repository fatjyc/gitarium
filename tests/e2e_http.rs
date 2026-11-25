/**
 * [INPUT]: 依赖 common 测试装置与真实 git 客户端
 * [OUTPUT]: 验证 HTTP 认证、首次推送建仓、克隆、标签、二进制、过期推送拒绝、空仓库与不存在仓库的行为
 * [POS]: HTTP 传输的端到端验收；不模拟协议字节，全部经由真实 git 命令
 * [PROTOCOL]: 变更时更新此头部，然后检查 CLAUDE.md
 */
mod common;

use base64::Engine;
use common::{commit_file, git, git_ok, http_url, init_repo, raw_status, start_server};

fn clone(dir: &std::path::Path, url: &str, into: &str) -> std::path::PathBuf {
    let target = dir.join(into);
    let output = git(dir, &["clone", "-q", url, target.to_str().unwrap()]);
    assert!(
        output.status.success(),
        "克隆失败: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    target
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn wrong_or_missing_password_gets_401() {
    let server = start_server().await;
    server.store.ensure_repo("demo").unwrap();
    let path = "/demo.git/info/refs?service=git-upload-pack";
    assert_eq!(raw_status(server.addr, path, None), 401);
    let wrong = base64::engine::general_purpose::STANDARD.encode("git:wrong");
    assert_eq!(
        raw_status(server.addr, path, Some(&format!("Basic {wrong}"))),
        401
    );
    let right = base64::engine::general_purpose::STANDARD.encode("git:git123");
    assert_eq!(
        raw_status(server.addr, path, Some(&format!("Basic {right}"))),
        200
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn first_push_creates_repository() {
    let server = start_server().await;
    let dir = tempfile::tempdir().unwrap();
    let work = dir.path().join("work");
    let head = init_repo(&work, "readme.txt", b"hello\n");
    let url = http_url(&server, "demo");

    let output = git(&work, &["push", &url, "main"]);
    assert!(
        output.status.success(),
        "推送失败: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    assert!(server.store.repo_exists("demo").unwrap());
    let refs = server.store.list_refs("demo").unwrap();
    assert_eq!(refs, vec![("refs/heads/main".to_string(), head.clone())]);

    let remote = common::git_ok(&work, &["ls-remote", &url]);
    assert!(
        remote.contains(&format!("{head}\trefs/heads/main")),
        "{remote}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn second_creation_of_same_branch_is_rejected() {
    let server = start_server().await;
    let dir = tempfile::tempdir().unwrap();
    let first = dir.path().join("first");
    let second = dir.path().join("second");
    let first_head = init_repo(&first, "a.txt", b"first\n");
    init_repo(&second, "a.txt", b"second\n");
    let url = http_url(&server, "demo");

    assert!(git(&first, &["push", &url, "main"]).status.success());
    let output = git(&second, &["push", &url, "main"]);
    assert!(!output.status.success(), "第二次创建应被拒绝");

    let refs = server.store.list_refs("demo").unwrap();
    assert_eq!(refs, vec![("refs/heads/main".to_string(), first_head)]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn clone_returns_same_commit_and_bytes() {
    let server = start_server().await;
    let dir = tempfile::tempdir().unwrap();
    let work = dir.path().join("work");
    let content = b"hello\n\xe4\xb8\xad\xe6\x96\x87\r\n".to_vec();
    let head = init_repo(&work, "readme.txt", &content);
    let url = http_url(&server, "demo");
    assert!(git(&work, &["push", &url, "main"]).status.success());

    let cloned = clone(dir.path(), &url, "cloned");
    assert_eq!(git_ok(&cloned, &["rev-parse", "HEAD"]).trim(), head);
    assert_eq!(std::fs::read(cloned.join("readme.txt")).unwrap(), content);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pull_after_new_push_updates_existing_clone() {
    let server = start_server().await;
    let dir = tempfile::tempdir().unwrap();
    let work = dir.path().join("work");
    init_repo(&work, "readme.txt", b"one\n");
    let url = http_url(&server, "demo");
    assert!(git(&work, &["push", &url, "main"]).status.success());
    let cloned = clone(dir.path(), &url, "cloned");

    let new_head = commit_file(&work, "readme.txt", b"two\n", "second");
    assert!(git(&work, &["push", &url, "main"]).status.success());
    git_ok(&cloned, &["pull", "-q", "--ff-only"]);
    assert_eq!(git_ok(&cloned, &["rev-parse", "HEAD"]).trim(), new_head);
    assert_eq!(
        std::fs::read(cloned.join("readme.txt")).unwrap(),
        b"two\n".to_vec()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn annotated_and_lightweight_tags_survive_clone() {
    let server = start_server().await;
    let dir = tempfile::tempdir().unwrap();
    let work = dir.path().join("work");
    init_repo(&work, "readme.txt", b"tagged\n");
    git_ok(&work, &["tag", "-a", "v1", "-m", "release one"]);
    git_ok(&work, &["tag", "light"]);
    let url = http_url(&server, "demo");
    let output = git(&work, &["push", &url, "main", "--tags"]);
    assert!(
        output.status.success(),
        "推送标签失败: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let cloned = clone(dir.path(), &url, "cloned");
    let tags = git_ok(&cloned, &["tag", "-l"]);
    assert!(tags.contains("v1"), "{tags}");
    assert!(tags.contains("light"), "{tags}");
    git_ok(&cloned, &["fsck", "--strict"]);
    let tag_type = git_ok(&cloned, &["cat-file", "-t", "v1"])
        .trim()
        .to_string();
    assert_eq!(tag_type, "tag");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn clone_preserves_binary_and_unterminated_last_line() {
    let server = start_server().await;
    let dir = tempfile::tempdir().unwrap();
    let work = dir.path().join("work");
    let content = b"\x00\x01line one\r\nline two\r\nno newline at end".to_vec();
    init_repo(&work, "data.bin", &content);
    let url = http_url(&server, "demo");
    assert!(git(&work, &["push", &url, "main"]).status.success());

    let cloned = clone(dir.path(), &url, "cloned");
    assert_eq!(std::fs::read(cloned.join("data.bin")).unwrap(), content);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stale_push_is_rejected_and_remote_keeps_first_update() {
    let server = start_server().await;
    let dir = tempfile::tempdir().unwrap();
    let work = dir.path().join("work");
    init_repo(&work, "readme.txt", b"base\n");
    let url = http_url(&server, "demo");
    assert!(git(&work, &["push", &url, "main"]).status.success());

    let first = clone(dir.path(), &url, "first");
    let second = clone(dir.path(), &url, "second");
    let first_head = commit_file(&first, "one.txt", b"first\n", "first change");
    commit_file(&second, "two.txt", b"second\n", "second change");

    assert!(git(&first, &["push", &url, "main"]).status.success());
    let rejected = git(&second, &["push", &url, "main"]);
    assert!(!rejected.status.success(), "过期推送应被拒绝");

    let refs = server.store.list_refs("demo").unwrap();
    assert_eq!(refs, vec![("refs/heads/main".to_string(), first_head)]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cloning_an_empty_repository_succeeds() {
    let server = start_server().await;
    server.store.ensure_repo("empty").unwrap();
    let dir = tempfile::tempdir().unwrap();
    let url = http_url(&server, "empty");
    let target = dir.path().join("empty-clone");
    let output = git(dir.path(), &["clone", "-q", &url, target.to_str().unwrap()]);
    assert!(
        output.status.success(),
        "空仓库克隆失败: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cloning_a_missing_repository_fails() {
    let server = start_server().await;
    let dir = tempfile::tempdir().unwrap();
    let url = http_url(&server, "missing");
    let target = dir.path().join("missing-clone");
    let output = git(dir.path(), &["clone", "-q", &url, target.to_str().unwrap()]);
    assert!(!output.status.success(), "克隆不存在的仓库应失败");
    assert!(!target.exists(), "失败的克隆不应留下目录");
}
