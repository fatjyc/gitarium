#![allow(dead_code)]
/**
 * [INPUT]: 依赖 gitdb 的 Config、Store、http::App、http::serve 与 ssh::serve；依赖标准 git 与 OpenSSH 客户端
 * [OUTPUT]: 导出 TestServer、HTTP/SSH 地址、askpass 脚本与 git/ssh 命令辅助函数
 * [POS]: 集成测试共用装置；每个测试独立数据库、随机端口与隔离的 Git 配置，SSH 认证经 askpass 自动应答
 * [PROTOCOL]: 变更时更新此头部，然后检查 CLAUDE.md
 */
use std::io::{Read, Write};
use std::net::SocketAddr;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::Arc;

use gitdb::config::Config;
use gitdb::http::{self, App};
use gitdb::ssh;
use gitdb::store::Store;

const SSH_COMMAND: &str = "ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -o PubkeyAuthentication=no -o PreferredAuthentications=password -o KbdInteractiveAuthentication=no -o NumberOfPasswordPrompts=3";

pub struct TestServer {
    pub addr: SocketAddr,
    pub ssh_addr: SocketAddr,
    pub askpass: PathBuf,
    pub store: Arc<Store>,
    _dir: Option<tempfile::TempDir>,
    task: tokio::task::JoinHandle<()>,
    ssh_task: tokio::task::JoinHandle<()>,
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.task.abort();
        self.ssh_task.abort();
    }
}

pub async fn start_server() -> TestServer {
    let dir = tempfile::tempdir().unwrap();
    let mut server = start_server_in(dir.path()).await;
    server._dir = Some(dir);
    server
}

pub async fn start_server_in(base: &Path) -> TestServer {
    let db_path = base.join("git.db");
    let askpass = base.join("askpass.sh");
    write_askpass(&askpass, "git123");
    let store = Arc::new(Store::open(db_path.to_str().unwrap()).unwrap());
    let config = Arc::new(Config::from_pairs(vec![
        ("GIT_USER", "git"),
        ("GIT_PASSWORD", "git123"),
        ("GIT_MAX_PUSH_BYTES", "67108864"),
    ]));
    let app = Arc::new(App {
        store: store.clone(),
        config: config.clone(),
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let _ = http::serve(app, listener).await;
    });
    let ssh_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ssh_addr = ssh_listener.local_addr().unwrap();
    let store_for_ssh = store.clone();
    let ssh_task = tokio::spawn(async move {
        let _ = ssh::serve(store_for_ssh, config, ssh_listener).await;
    });
    TestServer {
        addr,
        ssh_addr,
        askpass,
        store,
        _dir: None,
        task,
        ssh_task,
    }
}

pub fn write_askpass(path: &Path, password: &str) {
    std::fs::write(path, format!("#!/bin/sh\necho {password}\n")).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn command(cwd: &Path) -> Command {
    let mut command = Command::new("git");
    command
        .current_dir(cwd)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.com");
    command
}

pub fn git(cwd: &Path, args: &[&str]) -> Output {
    command(cwd).args(args).output().unwrap()
}

pub fn git_with_env(cwd: &Path, args: &[&str], envs: &[(&str, &str)]) -> Output {
    let mut cmd = command(cwd);
    cmd.args(args);
    for (key, value) in envs {
        cmd.env(key, value);
    }
    cmd.output().unwrap()
}

pub fn git_ok(cwd: &Path, args: &[&str]) -> String {
    let output = git(cwd, args);
    assert!(
        output.status.success(),
        "git {:?} 失败: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

pub fn ssh_env(server: &TestServer) -> [(&str, &str); 4] {
    [
        ("GIT_SSH_COMMAND", SSH_COMMAND),
        ("SSH_ASKPASS", server.askpass.to_str().unwrap()),
        ("SSH_ASKPASS_REQUIRE", "force"),
        ("DISPLAY", ":0"),
    ]
}

pub fn ssh_git(server: &TestServer, cwd: &Path, args: &[&str]) -> Output {
    git_with_env(cwd, args, &ssh_env(server))
}

pub fn ssh_command(server: &TestServer, askpass: &Path, remote: &str) -> Output {
    let port = server.ssh_addr.port().to_string();
    Command::new("ssh")
        .args([
            "-p",
            &port,
            "-o",
            "StrictHostKeyChecking=no",
            "-o",
            "UserKnownHostsFile=/dev/null",
            "-o",
            "LogLevel=ERROR",
            "-o",
            "PubkeyAuthentication=no",
            "-o",
            "PreferredAuthentications=password",
            "-o",
            "KbdInteractiveAuthentication=no",
            "-o",
            "NumberOfPasswordPrompts=1",
            "git@127.0.0.1",
            remote,
        ])
        .env("SSH_ASKPASS", askpass)
        .env("SSH_ASKPASS_REQUIRE", "force")
        .env("DISPLAY", ":0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .unwrap()
}

pub fn init_repo(dir: &Path, file: &str, content: &[u8]) -> String {
    std::fs::create_dir_all(dir).unwrap();
    git_ok(dir, &["init", "-q", "-b", "main"]);
    std::fs::write(dir.join(file), content).unwrap();
    git_ok(dir, &["add", file]);
    git_ok(dir, &["commit", "-q", "-m", "initial"]);
    git_ok(dir, &["rev-parse", "HEAD"]).trim().to_string()
}

pub fn commit_file(dir: &Path, file: &str, content: &[u8], message: &str) -> String {
    std::fs::write(dir.join(file), content).unwrap();
    git_ok(dir, &["add", file]);
    git_ok(dir, &["commit", "-q", "-m", message]);
    git_ok(dir, &["rev-parse", "HEAD"]).trim().to_string()
}

pub fn http_url(server: &TestServer, repo: &str) -> String {
    format!("http://git:git123@{}/{repo}.git", server.addr)
}

pub fn ssh_url(server: &TestServer, repo: &str) -> String {
    format!("ssh://git@127.0.0.1:{}/{repo}.git", server.ssh_addr.port())
}

pub fn raw_status(addr: SocketAddr, path: &str, authorization: Option<&str>) -> u16 {
    let mut stream = std::net::TcpStream::connect(addr).unwrap();
    let auth = authorization
        .map(|value| format!("Authorization: {value}\r\n"))
        .unwrap_or_default();
    let request =
        format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n{auth}\r\n");
    stream.write_all(request.as_bytes()).unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    response
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse().ok())
        .unwrap_or(0)
}
