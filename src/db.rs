/**
 * [INPUT]: 依赖 rusqlite、postgres 建立数据库连接；读取 DATABASE_URL 形式的路径或连接串
 * [OUTPUT]: 导出 Db（SQLite 互斥连接或 PostgreSQL 单工作线程）与 Db::open、Db::migrate
 * [POS]: 数据层入口；store 模块按后端分派 SQL。PostgreSQL 的同步客户端会自建运行时，因此只能在工作线程中调用
 * [PROTOCOL]: 变更时更新此头部，然后检查 CLAUDE.md
 */
use std::path::Path;
use std::sync::mpsc::{self, Receiver, Sender, SyncSender};
use std::sync::Mutex;
use std::thread;

use postgres::{Client, NoTls};
use rusqlite::Connection;

type PostgresTask = Box<dyn FnOnce(&mut Option<Client>) + Send>;

pub enum Db {
    Sqlite(Mutex<Connection>),
    Postgres { tasks: Mutex<Sender<PostgresTask>> },
}

impl Db {
    pub fn open(url: &str) -> Result<Db, String> {
        if url.starts_with("postgres://") || url.starts_with("postgresql://") {
            return Db::open_postgres(url);
        }

        let path = url.strip_prefix("sqlite://").unwrap_or(url);
        if let Some(parent) = Path::new(path).parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("创建数据目录 {} 失败: {e}", parent.display()))?;
            }
        }
        let connection =
            Connection::open(path).map_err(|e| format!("打开 SQLite {path} 失败: {e}"))?;
        connection
            .execute_batch("PRAGMA journal_mode=WAL; PRAGMA busy_timeout=5000;")
            .map_err(|e| format!("设置 SQLite 参数失败: {e}"))?;
        Ok(Db::Sqlite(Mutex::new(connection)))
    }

    fn open_postgres(url: &str) -> Result<Db, String> {
        let (task_tx, task_rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let worker_url = url.to_string();
        thread::Builder::new()
            .name("gitarium-postgres".to_string())
            .spawn(move || postgres_worker(&worker_url, task_rx, ready_tx))
            .map_err(|e| format!("启动 PostgreSQL 工作线程失败: {e}"))?;
        match ready_rx.recv() {
            Ok(Ok(())) => Ok(Db::Postgres {
                tasks: Mutex::new(task_tx),
            }),
            Ok(Err(error)) => Err(error),
            Err(_) => Err("PostgreSQL 工作线程启动失败".to_string()),
        }
    }

    pub fn migrate(&self) -> Result<(), String> {
        const SQLITE_SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS repos (
    name TEXT PRIMARY KEY,
    created_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS objects (
    repo TEXT NOT NULL,
    oid TEXT NOT NULL,
    kind INTEGER NOT NULL,
    data BLOB NOT NULL,
    PRIMARY KEY (repo, oid)
);
CREATE TABLE IF NOT EXISTS refs (
    repo TEXT NOT NULL,
    name TEXT NOT NULL,
    oid TEXT NOT NULL,
    PRIMARY KEY (repo, name)
);
CREATE TABLE IF NOT EXISTS settings (
    key TEXT PRIMARY KEY,
    value BLOB NOT NULL
);";
        const POSTGRES_SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS repos (
    name TEXT PRIMARY KEY,
    created_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS objects (
    repo TEXT NOT NULL,
    oid TEXT NOT NULL,
    kind INTEGER NOT NULL,
    data BYTEA NOT NULL,
    PRIMARY KEY (repo, oid)
);
CREATE TABLE IF NOT EXISTS refs (
    repo TEXT NOT NULL,
    name TEXT NOT NULL,
    oid TEXT NOT NULL,
    PRIMARY KEY (repo, name)
);
CREATE TABLE IF NOT EXISTS settings (
    key TEXT PRIMARY KEY,
    value BYTEA NOT NULL
);";
        match self {
            Db::Sqlite(connection) => {
                let guard = connection.lock().unwrap_or_else(|e| e.into_inner());
                guard
                    .execute_batch(SQLITE_SCHEMA)
                    .map_err(|e| format!("创建 SQLite 表失败: {e}"))?;
                drop_legacy_default_branch(&guard)
            }
            Db::Postgres { .. } => self.with_postgres(|client| {
                client.batch_execute(POSTGRES_SCHEMA)?;
                client.batch_execute("ALTER TABLE repos DROP COLUMN IF EXISTS default_branch")
            }),
        }
    }

    pub fn is_postgres(&self) -> bool {
        matches!(self, Db::Postgres { .. })
    }

    pub fn sqlite(&self) -> Option<std::sync::MutexGuard<'_, Connection>> {
        match self {
            Db::Sqlite(connection) => Some(connection.lock().unwrap_or_else(|e| e.into_inner())),
            Db::Postgres { .. } => None,
        }
    }

    pub fn with_postgres<T: Send + 'static>(
        &self,
        f: impl FnOnce(&mut Client) -> Result<T, postgres::Error> + Send + 'static,
    ) -> Result<T, String> {
        let Db::Postgres { tasks } = self else {
            return Err("当前数据库不是 PostgreSQL".to_string());
        };
        let (result_tx, result_rx) = mpsc::sync_channel(1);
        let task: PostgresTask = Box::new(move |client: &mut Option<Client>| {
            let result = match client.as_mut() {
                Some(client) => f(client).map_err(|error| format!("PostgreSQL 操作失败: {error}")),
                None => Err("PostgreSQL 连接不可用".to_string()),
            };
            let _ = result_tx.send(result);
        });
        tasks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .send(task)
            .map_err(|_| "PostgreSQL 工作线程已退出".to_string())?;
        result_rx
            .recv()
            .map_err(|_| "PostgreSQL 工作线程未返回结果".to_string())?
    }
}

fn drop_legacy_default_branch(connection: &Connection) -> Result<(), String> {
    let columns: Vec<String> = {
        let mut statement = connection
            .prepare("PRAGMA table_info(repos)")
            .map_err(|e| format!("读取 SQLite 表结构失败: {e}"))?;
        let rows = statement
            .query_map([], |row| row.get::<_, String>(1))
            .map_err(|e| format!("读取 SQLite 表结构失败: {e}"))?;
        let mut columns = Vec::new();
        for row in rows {
            columns.push(row.map_err(|e| format!("读取 SQLite 表结构失败: {e}"))?);
        }
        columns
    };
    if columns.iter().any(|name| name == "default_branch") {
        connection
            .execute("ALTER TABLE repos DROP COLUMN default_branch", [])
            .map_err(|e| format!("删除旧 default_branch 列失败: {e}"))?;
    }
    Ok(())
}

fn postgres_worker(
    url: &str,
    tasks: Receiver<PostgresTask>,
    ready: SyncSender<Result<(), String>>,
) {
    let mut client = match Client::connect(url, NoTls) {
        Ok(client) => {
            let _ = ready.send(Ok(()));
            Some(client)
        }
        Err(error) => {
            let _ = ready.send(Err(format!("连接 PostgreSQL 失败: {error}")));
            return;
        }
    };
    while let Ok(task) = tasks.recv() {
        if client
            .as_ref()
            .map(|client| client.is_closed())
            .unwrap_or(true)
        {
            match Client::connect(url, NoTls) {
                Ok(reconnected) => client = Some(reconnected),
                Err(error) => {
                    eprintln!("重连 PostgreSQL 失败: {error}");
                    client = None;
                }
            }
        }
        task(&mut client);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table_exists(db: &Db, name: &str) -> bool {
        let guard = db.sqlite().expect("测试使用 SQLite");
        let count: i64 = guard
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
                [name],
                |row| row.get(0),
            )
            .expect("查询 sqlite_master");
        count == 1
    }

    #[test]
    fn migrate_creates_all_tables() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(dir.path().join("git.db").to_str().unwrap()).unwrap();
        db.migrate().unwrap();
        for table in ["repos", "objects", "refs", "settings"] {
            assert!(table_exists(&db, table), "缺少表 {table}");
        }
    }

    #[test]
    fn migrate_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(dir.path().join("git.db").to_str().unwrap()).unwrap();
        db.migrate().unwrap();
        db.migrate().unwrap();
        assert!(table_exists(&db, "objects"));
    }

    #[test]
    fn migrate_drops_legacy_default_branch_column() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("git.db");
        {
            let connection = Connection::open(&path).unwrap();
            connection
                .execute_batch(
                    "CREATE TABLE repos (
                         name TEXT PRIMARY KEY,
                         default_branch TEXT NOT NULL DEFAULT 'main',
                         created_at TEXT NOT NULL
                     );
                     INSERT INTO repos(name, created_at) VALUES ('demo', '1');",
                )
                .unwrap();
        }
        let db = Db::open(path.to_str().unwrap()).unwrap();
        db.migrate().unwrap();
        let guard = db.sqlite().unwrap();
        let count: i64 = guard
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('repos') WHERE name = 'default_branch'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 0, "旧列应被删除");
        let name: String = guard
            .query_row("SELECT name FROM repos", [], |row| row.get(0))
            .unwrap();
        assert_eq!(name, "demo", "仓库行应保留");
    }
}
