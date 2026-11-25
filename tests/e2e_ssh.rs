/**
 * [INPUT]: 依赖 common 测试装置、真实 git 客户端与 OpenSSH 客户端，依赖 gitarium::ssh 启动独立 SSH 服务
 * [OUTPUT]: 验证 SSH 与 HTTP 双向互操作、密码认证、命令白名单、不存在仓库失败与主机密钥持久化
 * [POS]: SSH 传输的端到端验收；不模拟协议字节，全部经由真实 git 与 ssh 命令
 * [PROTOCOL]: 变更时更新此头部，然后检查 CLAUDE.md
 */
mod common;

use std::sync::Arc;
use std::time::Duration;

use common::{
    git, git_ok, http_url, init_repo, ssh_command, ssh_git, ssh_url, start_server, write_askpass,
};
use gitarium::config::Config;
use gitarium::{ssh, store::Store};
use tokio::net::TcpListener;

fn clone_ok(cwd: &std::path::Path, args: &[&str]) -> std::path::PathBuf {
    let output = git(cwd, args);
    assert!(
        output.status.success(),
        "克隆失败: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let target = args.last().unwrap();
    cwd.join(target)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ssh_push_then_http_clone_sees_same_history() {
    let server = start_server().await;
    let dir = tempfile::tempdir().unwrap();
    let work = dir.path().join("work");
    let content = b"ssh first\n\xe4\xb8\xad\xe6\x96\x87\r\n".to_vec();
    let head = init_repo(&work, "readme.txt", &content);
    git_ok(&work, &["tag", "-a", "v1", "-m", "release one"]);
    let url = ssh_url(&server, "demo");

    let output = ssh_git(&server, &work, &["push", &url, "main", "--tags"]);
    assert!(
        output.status.success(),
        "SSH 推送失败: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        server.store.get_ref("demo", "refs/heads/main").unwrap(),
        Some(head.clone())
    );

    let clone_url = http_url(&server, "demo");
    let target = "cloned";
    let cloned = clone_ok(dir.path(), &["clone", "-q", &clone_url, target]);
    assert_eq!(git_ok(&cloned, &["rev-parse", "HEAD"]).trim(), head);
    assert_eq!(std::fs::read(cloned.join("readme.txt")).unwrap(), content);
    assert!(git_ok(&cloned, &["tag", "-l"]).contains("v1"));
    assert_eq!(git_ok(&cloned, &["cat-file", "-t", "v1"]).trim(), "tag");
    git_ok(&cloned, &["fsck", "--strict"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn http_push_then_ssh_clone_sees_same_history() {
    let server = start_server().await;
    let dir = tempfile::tempdir().unwrap();
    let work = dir.path().join("work");
    let content = b"http first\nline two\n".to_vec();
    let head = init_repo(&work, "readme.txt", &content);
    git_ok(&work, &["tag", "light"]);
    let url = http_url(&server, "demo");

    let output = git(&work, &["push", &url, "main", "--tags"]);
    assert!(
        output.status.success(),
        "HTTP 推送失败: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let clone_url = ssh_url(&server, "demo");
    let target = "cloned";
    let output = ssh_git(&server, dir.path(), &["clone", "-q", &clone_url, target]);
    assert!(
        output.status.success(),
        "SSH 克隆失败: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let cloned = dir.path().join(target);
    assert_eq!(git_ok(&cloned, &["rev-parse", "HEAD"]).trim(), head);
    assert_eq!(std::fs::read(cloned.join("readme.txt")).unwrap(), content);
    assert!(git_ok(&cloned, &["tag", "-l"]).contains("light"));
    git_ok(&cloned, &["fsck", "--strict"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn wrong_password_is_rejected() {
    let server = start_server().await;
    let dir = tempfile::tempdir().unwrap();
    let wrong = dir.path().join("wrong-askpass.sh");
    write_askpass(&wrong, "not-the-password");

    let output = ssh_command(&server, &wrong, "git-upload-pack '/demo.git'");
    assert!(!output.status.success(), "错误密码不应通过认证");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn non_git_command_is_rejected() {
    let server = start_server().await;
    let output = ssh_command(&server, &server.askpass, "echo hello");
    assert!(!output.status.success(), "非 Git 命令应被拒绝");
    assert!(output.stdout.is_empty(), "被拒绝的命令不应产生输出");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cloning_a_missing_repository_over_ssh_fails() {
    let server = start_server().await;
    let dir = tempfile::tempdir().unwrap();
    let url = ssh_url(&server, "missing");
    let output = ssh_git(&server, dir.path(), &["clone", "-q", &url, "missing-clone"]);
    assert!(!output.status.success(), "克隆不存在的仓库应失败");
    assert!(!dir.path().join("missing-clone").exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn host_key_survives_database_restart() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("git.db");
    let db = db.to_str().unwrap().to_string();
    let config = Arc::new(Config::from_pairs(vec![
        ("GIT_USER", "git"),
        ("GIT_PASSWORD", "git123"),
    ]));

    let first = {
        let store = Arc::new(Store::open(&db).unwrap());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let store_for_task = store.clone();
        let config_for_task = config.clone();
        let task = tokio::spawn(async move {
            let _ = ssh::serve(store_for_task, config_for_task, listener).await;
        });
        let key = scan_host_key(port).await;
        task.abort();
        key
    };

    let second = {
        let store = Arc::new(Store::open(&db).unwrap());
        assert!(
            store.get_setting("ssh_host_key").unwrap().is_some(),
            "主机密钥应保存在设置表"
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let task = tokio::spawn(async move {
            let _ = ssh::serve(store, config, listener).await;
        });
        let key = scan_host_key(port).await;
        task.abort();
        key
    };

    assert_eq!(first, second, "重启后主机密钥应保持不变");
}

async fn scan_host_key(port: u16) -> String {
    let port = port.to_string();
    for _ in 0..50 {
        let output = tokio::process::Command::new("ssh-keyscan")
            .args(["-p", &port, "-t", "ed25519", "127.0.0.1"])
            .output()
            .await
            .unwrap();
        if output.status.success() {
            let text = String::from_utf8_lossy(&output.stdout);
            if let Some(blob) = text
                .lines()
                .filter(|line| !line.starts_with('#'))
                .find_map(|line| line.split_whitespace().nth(2))
            {
                return blob.to_string();
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("ssh-keyscan 未能取到主机密钥");
}
