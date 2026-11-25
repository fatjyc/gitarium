/**
 * [INPUT]: 依赖 russh 提供 SSH 传输；依赖 advertise、fetch、push 协议核心；依赖 Store 持久化主机密钥
 * [OUTPUT]: 导出 serve，启动密码认证的 SSH Git 服务，只接受 git-upload-pack 与 git-receive-pack
 * [POS]: SSH 传输层；把 SSH 通道字节桥接给同步协议核心，自身不承载 Git 语义
 * [PROTOCOL]: 变更时更新此头部，然后检查 CLAUDE.md
 */
use std::io::{self, BufRead, BufReader, Read, Write};
use std::sync::mpsc::{self as std_mpsc, Receiver, Sender};
use std::sync::Arc;
use std::time::Duration;

use russh::keys::ssh_key::LineEnding;
use russh::keys::{Algorithm, PrivateKey};
use russh::server::{self, Auth, Handler, Msg, Server as _};
use russh::{Channel, ChannelId, MethodKind, MethodSet};
use tokio::sync::mpsc as tokio_mpsc;

use crate::config::Config;
use crate::git::advertise::{self, Service};
use crate::git::{fetch, push};
use crate::store::{validate_repo_name, Store};

const HOST_KEY_SETTING: &str = "ssh_host_key";
const OUTPUT_CHUNK: usize = 32 * 1024;
const OUTPUT_QUEUE: usize = 8;

// ============================================================
// 服务启动
// ============================================================

pub async fn serve(
    store: Arc<Store>,
    config: Arc<Config>,
    listener: tokio::net::TcpListener,
) -> Result<(), String> {
    let key = host_key(&store)?;
    let mut methods = MethodSet::empty();
    methods.push(MethodKind::Password);
    let ssh_config = Arc::new(server::Config {
        methods,
        auth_rejection_time_initial: Some(Duration::ZERO),
        keys: vec![key],
        ..Default::default()
    });
    let mut server = SshServer { store, config };
    server
        .run_on_socket(ssh_config, &listener)
        .await
        .map_err(|error| format!("SSH 服务退出: {error}"))
}

fn host_key(store: &Store) -> Result<PrivateKey, String> {
    if let Some(bytes) = store.get_setting(HOST_KEY_SETTING)? {
        let text = String::from_utf8(bytes).map_err(|_| "SSH 主机密钥不是 UTF-8".to_string())?;
        return PrivateKey::from_openssh(text.as_bytes())
            .map_err(|error| format!("解析 SSH 主机密钥失败: {error}"));
    }
    let key = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519)
        .map_err(|error| format!("生成 SSH 主机密钥失败: {error}"))?;
    let text = key
        .to_openssh(LineEnding::LF)
        .map_err(|error| format!("编码 SSH 主机密钥失败: {error}"))?;
    store.put_setting(HOST_KEY_SETTING, text.as_bytes())?;
    Ok(key)
}

// ============================================================
// 连接与命令
// ============================================================

struct SshServer {
    store: Arc<Store>,
    config: Arc<Config>,
}

impl server::Server for SshServer {
    type Handler = Connection;

    fn new_client(&mut self, _peer: Option<std::net::SocketAddr>) -> Connection {
        Connection {
            store: self.store.clone(),
            config: self.config.clone(),
            inbound: None,
            channel: None,
        }
    }
}

struct Connection {
    store: Arc<Store>,
    config: Arc<Config>,
    inbound: Option<Sender<Vec<u8>>>,
    channel: Option<ChannelId>,
}

impl Handler for Connection {
    type Error = russh::Error;

    async fn auth_password(&mut self, user: &str, password: &str) -> Result<Auth, Self::Error> {
        if user == self.config.git_user && password == self.config.git_password {
            Ok(Auth::Accept)
        } else {
            Ok(Auth::reject())
        }
    }

    async fn channel_open_session(
        &mut self,
        _channel: Channel<Msg>,
        reply: server::ChannelOpenHandle,
        _session: &mut server::Session,
    ) -> Result<(), Self::Error> {
        reply.accept().await;
        Ok(())
    }

    async fn exec_request(
        &mut self,
        channel: ChannelId,
        data: &[u8],
        session: &mut server::Session,
    ) -> Result<(), Self::Error> {
        let Some((service, repo)) = parse_command(data) else {
            session.channel_failure(channel)?;
            return Ok(());
        };
        if self.inbound.is_some() {
            session.channel_failure(channel)?;
            return Ok(());
        }
        session.channel_success(channel)?;
        self.channel = Some(channel);

        let (input_tx, input_rx) = std_mpsc::channel();
        self.inbound = Some(input_tx);
        let (output_tx, mut output_rx) = tokio_mpsc::channel::<StreamMessage>(OUTPUT_QUEUE);
        let handle = session.handle();
        tokio::spawn(async move {
            let mut exit_code = 0;
            while let Some(message) = output_rx.recv().await {
                match message {
                    StreamMessage::Data(chunk) => {
                        if handle.data(channel, chunk).await.is_err() {
                            break;
                        }
                    }
                    StreamMessage::Exit(code) => exit_code = code,
                }
            }
            let _ = handle.exit_status_request(channel, exit_code).await;
            let _ = handle.eof(channel).await;
            let _ = handle.close(channel).await;
        });

        let store = self.store.clone();
        let max = self.config.max_push_bytes;
        tokio::task::spawn_blocking(move || {
            let mut reader = BufReader::new(ChannelReader::new(input_rx));
            let mut writer = ChannelWriter::new(output_tx);
            let result = run_protocol(&store, &repo, service, &mut reader, &mut writer, max);
            if let Err(error) = &result {
                eprintln!("SSH {} 处理失败: {error}", service.name());
            }
            let _ = writer.flush();
            writer.finish(if result.is_ok() { 0 } else { 1 });
        });
        Ok(())
    }

    async fn data(
        &mut self,
        channel: ChannelId,
        data: &[u8],
        _session: &mut server::Session,
    ) -> Result<(), Self::Error> {
        if self.channel == Some(channel) {
            if let Some(sender) = &self.inbound {
                let _ = sender.send(data.to_vec());
            }
        }
        Ok(())
    }

    async fn channel_eof(
        &mut self,
        channel: ChannelId,
        _session: &mut server::Session,
    ) -> Result<(), Self::Error> {
        if self.channel == Some(channel) {
            self.inbound.take();
        }
        Ok(())
    }

    async fn channel_close(
        &mut self,
        channel: ChannelId,
        _session: &mut server::Session,
    ) -> Result<(), Self::Error> {
        if self.channel == Some(channel) {
            self.inbound.take();
        }
        Ok(())
    }
}

fn parse_command(data: &[u8]) -> Option<(Service, String)> {
    let text = std::str::from_utf8(data)
        .ok()?
        .trim_end_matches(['\r', '\n']);
    let (name, rest) = text.split_once(' ')?;
    let service = match name {
        "git-upload-pack" => Service::UploadPack,
        "git-receive-pack" => Service::ReceivePack,
        _ => return None,
    };
    let path = rest.trim();
    let path = path
        .strip_prefix('\'')
        .and_then(|inner| inner.strip_suffix('\''))
        .unwrap_or(path);
    let path = path.trim_start_matches('/');
    let name = path.strip_suffix(".git").unwrap_or(path);
    validate_repo_name(name).ok()?;
    Some((service, name.to_string()))
}

fn run_protocol(
    store: &Store,
    repo: &str,
    service: Service,
    input: &mut dyn BufRead,
    out: &mut dyn Write,
    max: usize,
) -> Result<(), String> {
    let advertisement = advertise::refs(store, repo, service)?;
    out.write_all(&advertisement)
        .map_err(|error| format!("写引用通告失败: {error}"))?;
    out.flush()
        .map_err(|error| format!("发送引用通告失败: {error}"))?;
    match service {
        Service::UploadPack => fetch::upload(store, repo, input, out, max),
        Service::ReceivePack => push::receive(store, repo, input, out, max),
    }
}

// ============================================================
// 同步协议核心与异步 SSH 会话之间的字节桥
// ============================================================

struct ChannelReader {
    input: Receiver<Vec<u8>>,
    buffer: Vec<u8>,
    position: usize,
}

impl ChannelReader {
    fn new(input: Receiver<Vec<u8>>) -> ChannelReader {
        ChannelReader {
            input,
            buffer: Vec::new(),
            position: 0,
        }
    }
}

impl Read for ChannelReader {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        while self.position >= self.buffer.len() {
            match self.input.recv() {
                Ok(chunk) => {
                    self.buffer = chunk;
                    self.position = 0;
                }
                Err(_) => return Ok(0),
            }
        }
        let available = &self.buffer[self.position..];
        let count = available.len().min(out.len());
        out[..count].copy_from_slice(&available[..count]);
        self.position += count;
        Ok(count)
    }
}

struct ChannelWriter {
    output: tokio_mpsc::Sender<StreamMessage>,
    buffer: Vec<u8>,
}

enum StreamMessage {
    Data(Vec<u8>),
    Exit(u32),
}

impl ChannelWriter {
    fn new(output: tokio_mpsc::Sender<StreamMessage>) -> ChannelWriter {
        ChannelWriter {
            output,
            buffer: Vec::new(),
        }
    }

    fn finish(&mut self, code: u32) {
        let _ = self.output.blocking_send(StreamMessage::Exit(code));
    }
}

impl Write for ChannelWriter {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        self.buffer.extend_from_slice(data);
        if self.buffer.len() >= OUTPUT_CHUNK {
            self.flush()?;
        }
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        if self.buffer.is_empty() {
            return Ok(());
        }
        let chunk = std::mem::take(&mut self.buffer);
        self.output
            .blocking_send(StreamMessage::Data(chunk))
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "SSH 通道已关闭"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upload_pack_accepts_quoted_absolute_path() {
        let parsed = parse_command(b"git-upload-pack '/demo.git'").unwrap();
        assert_eq!(parsed.0, Service::UploadPack);
        assert_eq!(parsed.1, "demo");
    }

    #[test]
    fn receive_pack_accepts_unquoted_relative_path() {
        let parsed = parse_command(b"git-receive-pack 'demo.git'").unwrap();
        assert_eq!(parsed.0, Service::ReceivePack);
        assert_eq!(parsed.1, "demo");
    }

    #[test]
    fn repository_suffix_is_optional() {
        assert_eq!(
            parse_command(b"git-receive-pack '/demo'").unwrap().1,
            "demo"
        );
    }

    #[test]
    fn non_git_command_is_rejected() {
        assert!(parse_command(b"echo hello").is_none());
        assert!(parse_command(b"git-upload-pack").is_none());
    }

    #[test]
    fn nested_or_escaping_path_is_rejected() {
        assert!(parse_command(b"git-upload-pack '/team/demo.git'").is_none());
        assert!(parse_command(b"git-upload-pack '/../demo.git'").is_none());
    }

    #[test]
    fn empty_or_odd_command_is_rejected() {
        assert!(parse_command(b"").is_none());
        assert!(parse_command(b"git-receive-pack '  '").is_none());
    }
}
