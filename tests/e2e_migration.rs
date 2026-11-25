/**
 * [INPUT]: 依赖 common 测试装置、真实 git 客户端与 SQLite 数据文件
 * [OUTPUT]: 验证复制数据库文件后历史、标签与 fsck 一致，并能继续推送；验证同一数据库重启
 * [POS]: SQLite 迁移验收；迁移单位是一个数据库文件，不依赖任何裸仓库目录
 * [PROTOCOL]: 变更时更新此头部，然后检查 CLAUDE.md
 */
mod common;

use std::path::{Path, PathBuf};
use std::time::Duration;

use common::{commit_file, git, git_ok, http_url, init_repo, ssh_git, ssh_url, start_server_in};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sqlite_file_copy_restores_history_and_accepts_new_push() {
    let origin = tempfile::tempdir().unwrap();
    let restored = tempfile::tempdir().unwrap();
    let work = origin.path().join("work");
    let content = b"before migration\n\xe4\xb8\xad\xe6\x96\x87\r\n".to_vec();
    let first_head = init_repo(&work, "readme.txt", &content);
    git_ok(&work, &["tag", "-a", "v1", "-m", "before migration"]);
    git_ok(&work, &["tag", "light"]);

    {
        let server = start_server_in(origin.path()).await;
        let url = http_url(&server, "demo");
        let output = git(&work, &["push", &url, "main", "--tags"]);
        assert!(
            output.status.success(),
            "推送失败: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let remote = git_ok(&work, &["ls-remote", &url]);
        assert!(remote.contains(&format!("{first_head}\trefs/heads/main")));
    }
    let origin_db = origin.path().join("git.db");
    wait_until_closed(&origin_db).await;

    let restored_db = restored.path().join("git.db");
    std::fs::copy(&origin_db, &restored_db).unwrap();

    let server = start_server_in(restored.path()).await;
    let url = http_url(&server, "demo");
    let clone_dir = restored.path().join("clone");
    let output = git(
        restored.path(),
        &["clone", "-q", &url, clone_dir.to_str().unwrap()],
    );
    assert!(
        output.status.success(),
        "恢复后克隆失败: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        git_ok(&clone_dir, &["rev-parse", "HEAD"]).trim(),
        first_head
    );
    assert_eq!(
        std::fs::read(clone_dir.join("readme.txt")).unwrap(),
        content
    );
    let tags = git_ok(&clone_dir, &["tag", "-l"]);
    assert!(tags.contains("v1"), "{tags}");
    assert!(tags.contains("light"), "{tags}");
    assert_eq!(git_ok(&clone_dir, &["cat-file", "-t", "v1"]).trim(), "tag");
    git_ok(&clone_dir, &["fsck", "--strict"]);

    let new_head = commit_file(
        &clone_dir,
        "readme.txt",
        b"after migration\n",
        "after migration",
    );
    let output = git(&clone_dir, &["push", &url, "main"]);
    assert!(
        output.status.success(),
        "恢复后推送失败: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    drop(server);
    wait_until_closed(&restored_db).await;
    let server = start_server_in(restored.path()).await;
    let ssh_clone = restored.path().join("ssh-clone");
    let ssh_clone_url = ssh_url(&server, "demo");
    let output = ssh_git(
        &server,
        restored.path(),
        &["clone", "-q", &ssh_clone_url, ssh_clone.to_str().unwrap()],
    );
    assert!(
        output.status.success(),
        "重启后 SSH 克隆失败: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(git_ok(&ssh_clone, &["rev-parse", "HEAD"]).trim(), new_head);
    git_ok(&ssh_clone, &["fsck", "--strict"]);
}

async fn wait_until_closed(db: &Path) {
    let wal = PathBuf::from(format!("{}-wal", db.display()));
    for _ in 0..200 {
        if !wal.exists() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("数据库连接未在超时内关闭，无法复制一致的数据库文件");
}
