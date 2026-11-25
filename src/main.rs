/**
 * [INPUT]: 依赖 Config 读取环境变量，依赖 Store 打开数据库，依赖 http::serve 与 ssh::serve 启动两个入口
 * [OUTPUT]: 可执行程序 gitdb；同时监听 HTTP 与 SSH，任一入口退出即报错退出
 * [POS]: 进程入口；数据库必须在异步运行时之外打开，postgres 同步客户端不能在运行时内建立连接
 * [PROTOCOL]: 变更时更新此头部，然后检查 CLAUDE.md
 */
use std::sync::Arc;

use gitdb::config::Config;
use gitdb::http::{self, App};
use gitdb::ssh;
use gitdb::store::Store;

fn main() {
    let config = Config::from_env();
    let store = match Store::open(&config.database_url) {
        Ok(store) => Arc::new(store),
        Err(error) => {
            eprintln!("启动失败: {error}");
            std::process::exit(1);
        }
    };
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("创建异步运行时失败: {error}");
            std::process::exit(1);
        }
    };
    if let Err(error) = runtime.block_on(run(config, store)) {
        eprintln!("启动失败: {error}");
        std::process::exit(1);
    }
}

async fn run(config: Config, store: Arc<Store>) -> Result<(), String> {
    let http_listener = tokio::net::TcpListener::bind(&config.http_addr)
        .await
        .map_err(|error| format!("绑定 HTTP 地址 {} 失败: {error}", config.http_addr))?;
    let ssh_listener = tokio::net::TcpListener::bind(&config.ssh_addr)
        .await
        .map_err(|error| format!("绑定 SSH 地址 {} 失败: {error}", config.ssh_addr))?;
    println!("数据库: {}", config.database_url);
    println!(
        "HTTP 监听: {}",
        http_listener
            .local_addr()
            .map_err(|error| format!("读取 HTTP 地址失败: {error}"))?
    );
    println!(
        "SSH 监听: {}",
        ssh_listener
            .local_addr()
            .map_err(|error| format!("读取 SSH 地址失败: {error}"))?
    );

    let config = Arc::new(config);
    let app = Arc::new(App {
        store: store.clone(),
        config: config.clone(),
    });
    let http_task = tokio::spawn(http::serve(app, http_listener));
    let ssh_task = tokio::spawn(ssh::serve(store, config, ssh_listener));
    tokio::select! {
        result = http_task => result.map_err(|error| format!("HTTP 任务失败: {error}"))?,
        result = ssh_task => result.map_err(|error| format!("SSH 任务失败: {error}"))?,
    }
}
