/**
 * [INPUT]: 依赖 db::Db 的 SQLite 连接与 PostgreSQL 连接；依赖 git::GitKind 的对象类型编号
 * [OUTPUT]: 导出 Store 的对象、引用、设置读写方法与仓库生命周期方法（列表、详情、创建、改名、删除）；引用更新为比较并交换语义
 * [POS]: 数据层唯一业务接口；协议核心与管理接口只通过本模块接触数据库，不直接写 SQL
 * [PROTOCOL]: 变更时更新此头部，然后检查 CLAUDE.md
 */
use crate::db::Db;
use rusqlite::{params, OptionalExtension};

fn now_text() -> String {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs().to_string())
        .unwrap_or_default()
}

fn sqlite_error(context: &str, error: rusqlite::Error) -> String {
    format!("{context}: {error}")
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum GitKind {
    Commit = 1,
    Tree = 2,
    Blob = 3,
    Tag = 4,
}

impl GitKind {
    pub fn from_number(value: i64) -> Option<GitKind> {
        match value {
            1 => Some(GitKind::Commit),
            2 => Some(GitKind::Tree),
            3 => Some(GitKind::Blob),
            4 => Some(GitKind::Tag),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            GitKind::Commit => "commit",
            GitKind::Tree => "tree",
            GitKind::Blob => "blob",
            GitKind::Tag => "tag",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitObject {
    pub kind: GitKind,
    pub data: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RepoInfo {
    pub name: String,
    pub created_at: String,
}

#[derive(Debug, PartialEq, Eq)]
pub enum RepoUpdateError {
    NotFound,
    Conflict,
    InvalidName(String),
    Database(String),
}

enum UpdateOutcome {
    Updated,
    NotFound,
    Conflict,
}

pub struct Store {
    db: Db,
}

impl Store {
    pub fn new(db: Db) -> Store {
        Store { db }
    }

    pub fn open(url: &str) -> Result<Store, String> {
        let db = Db::open(url)?;
        db.migrate()?;
        Ok(Store::new(db))
    }

    pub fn ensure_repo(&self, repo: &str) -> Result<(), String> {
        validate_repo_name(repo)?;
        let name = repo.strip_suffix(".git").unwrap_or(repo);
        if let Some(guard) = self.db.sqlite() {
            guard
                .execute(
                    "INSERT OR IGNORE INTO repos(name, created_at) VALUES (?1, ?2)",
                    params![name, now_text()],
                )
                .map_err(|e| sqlite_error("创建仓库失败", e))?;
            return Ok(());
        }
        let name = name.to_string();
        let now = now_text();
        self.db.with_postgres(move |client| {
            client.execute(
                "INSERT INTO repos(name, created_at) VALUES ($1, $2) ON CONFLICT(name) DO NOTHING",
                &[&name, &now],
            )
        })?;
        Ok(())
    }

    pub fn repo_exists(&self, repo: &str) -> Result<bool, String> {
        let name = repo.strip_suffix(".git").unwrap_or(repo);
        if let Some(guard) = self.db.sqlite() {
            return guard
                .query_row("SELECT 1 FROM repos WHERE name = ?1", [name], |row| {
                    row.get::<_, i64>(0)
                })
                .optional()
                .map(|row| row.is_some())
                .map_err(|e| sqlite_error("查询仓库失败", e));
        }
        let name = name.to_string();
        let rows = self.db.with_postgres(move |client| {
            client.query("SELECT 1 FROM repos WHERE name = $1", &[&name])
        })?;
        Ok(!rows.is_empty())
    }

    pub fn list_repos(&self) -> Result<Vec<RepoInfo>, String> {
        if let Some(guard) = self.db.sqlite() {
            let mut statement = guard
                .prepare("SELECT name, created_at FROM repos ORDER BY name")
                .map_err(|e| sqlite_error("查询仓库列表失败", e))?;
            let rows = statement
                .query_map([], |row| {
                    Ok(RepoInfo {
                        name: row.get(0)?,
                        created_at: row.get(1)?,
                    })
                })
                .map_err(|e| sqlite_error("查询仓库列表失败", e))?;
            return rows
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| sqlite_error("读取仓库列表失败", e));
        }
        let rows = self.db.with_postgres(|client| {
            client.query("SELECT name, created_at FROM repos ORDER BY name", &[])
        })?;
        Ok(rows
            .iter()
            .map(|row| RepoInfo {
                name: row.get(0),
                created_at: row.get(1),
            })
            .collect())
    }

    pub fn get_repo(&self, repo: &str) -> Result<Option<RepoInfo>, String> {
        let name = normalize_repo_name(repo)?;
        if let Some(guard) = self.db.sqlite() {
            return guard
                .query_row(
                    "SELECT name, created_at FROM repos WHERE name = ?1",
                    [&name],
                    |row| {
                        Ok(RepoInfo {
                            name: row.get(0)?,
                            created_at: row.get(1)?,
                        })
                    },
                )
                .optional()
                .map_err(|e| sqlite_error("查询仓库失败", e));
        }
        let rows = self.db.with_postgres(move |client| {
            client.query(
                "SELECT name, created_at FROM repos WHERE name = $1",
                &[&name],
            )
        })?;
        Ok(rows.first().map(|row| RepoInfo {
            name: row.get(0),
            created_at: row.get(1),
        }))
    }

    pub fn create_repo(&self, repo: &str) -> Result<bool, String> {
        let name = normalize_repo_name(repo)?;
        let now = now_text();
        if let Some(guard) = self.db.sqlite() {
            let changed = guard
                .execute(
                    "INSERT OR IGNORE INTO repos(name, created_at) VALUES (?1, ?2)",
                    params![name, now],
                )
                .map_err(|e| sqlite_error("创建仓库失败", e))?;
            return Ok(changed == 1);
        }
        let changed = self.db.with_postgres(move |client| {
            client.execute(
                "INSERT INTO repos(name, created_at) VALUES ($1, $2) ON CONFLICT(name) DO NOTHING",
                &[&name, &now],
            )
        })?;
        Ok(changed == 1)
    }

    pub fn update_repo(&self, repo: &str, new_name: Option<&str>) -> Result<(), RepoUpdateError> {
        let old = normalize_repo_name(repo).map_err(RepoUpdateError::InvalidName)?;
        let new_name = match new_name {
            Some(value) => Some(normalize_repo_name(value).map_err(RepoUpdateError::InvalidName)?),
            None => None,
        };
        let rename = new_name.filter(|value| value != &old);
        let outcome = if let Some(mut guard) = self.db.sqlite() {
            update_repo_sqlite(&mut guard, &old, rename.as_deref())
                .map_err(|e| RepoUpdateError::Database(sqlite_error("更新仓库失败", e)))?
        } else {
            let old_for_db = old.clone();
            let rename_for_db = rename.clone();
            self.db
                .with_postgres(move |client| {
                    update_repo_postgres(client, &old_for_db, rename_for_db.as_deref())
                })
                .map_err(RepoUpdateError::Database)?
        };
        match outcome {
            UpdateOutcome::Updated => Ok(()),
            UpdateOutcome::NotFound => Err(RepoUpdateError::NotFound),
            UpdateOutcome::Conflict => Err(RepoUpdateError::Conflict),
        }
    }

    pub fn delete_repo(&self, repo: &str) -> Result<bool, String> {
        let name = normalize_repo_name(repo)?;
        if let Some(mut guard) = self.db.sqlite() {
            let tx = guard
                .transaction()
                .map_err(|e| sqlite_error("开启事务失败", e))?;
            tx.execute("DELETE FROM refs WHERE repo = ?1", params![name])
                .map_err(|e| sqlite_error("删除仓库引用失败", e))?;
            tx.execute("DELETE FROM objects WHERE repo = ?1", params![name])
                .map_err(|e| sqlite_error("删除仓库对象失败", e))?;
            let changed = tx
                .execute("DELETE FROM repos WHERE name = ?1", params![name])
                .map_err(|e| sqlite_error("删除仓库失败", e))?;
            tx.commit().map_err(|e| sqlite_error("提交事务失败", e))?;
            return Ok(changed == 1);
        }
        let changed = self.db.with_postgres(move |client| {
            let mut tx = client.transaction()?;
            tx.execute("DELETE FROM refs WHERE repo = $1", &[&name])?;
            tx.execute("DELETE FROM objects WHERE repo = $1", &[&name])?;
            let changed = tx.execute("DELETE FROM repos WHERE name = $1", &[&name])?;
            tx.commit()?;
            Ok(changed)
        })?;
        Ok(changed == 1)
    }

    pub fn put_object(
        &self,
        repo: &str,
        oid: &str,
        kind: GitKind,
        data: &[u8],
    ) -> Result<(), String> {
        let name = repo.strip_suffix(".git").unwrap_or(repo);
        if let Some(guard) = self.db.sqlite() {
            guard
                .execute(
                    "INSERT INTO objects(repo, oid, kind, data) VALUES (?1, ?2, ?3, ?4) ON CONFLICT(repo, oid) DO NOTHING",
                    params![name, oid, kind as i64, data],
                )
                .map_err(|e| sqlite_error("写入对象失败", e))?;
            return Ok(());
        }
        let name = name.to_string();
        let oid = oid.to_string();
        let kind_number = kind as i32;
        let data = data.to_vec();
        self.db.with_postgres(move |client| {
            client.execute(
                "INSERT INTO objects(repo, oid, kind, data) VALUES ($1, $2, $3, $4) ON CONFLICT(repo, oid) DO NOTHING",
                &[&name, &oid, &kind_number, &data],
            )
        })?;
        Ok(())
    }

    pub fn get_object(&self, repo: &str, oid: &str) -> Result<Option<GitObject>, String> {
        let name = repo.strip_suffix(".git").unwrap_or(repo);
        if let Some(guard) = self.db.sqlite() {
            let row: Option<(i64, Vec<u8>)> = guard
                .query_row(
                    "SELECT kind, data FROM objects WHERE repo = ?1 AND oid = ?2",
                    params![name, oid],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()
                .map_err(|e| sqlite_error("读取对象失败", e))?;
            return match row {
                Some((kind, data)) => GitKind::from_number(kind)
                    .map(|kind| Some(GitObject { kind, data }))
                    .ok_or_else(|| format!("对象 {oid} 的类型编号 {kind} 无效")),
                None => Ok(None),
            };
        }
        let name = name.to_string();
        let oid = oid.to_string();
        let query_oid = oid.clone();
        let rows = self.db.with_postgres(move |client| {
            client.query(
                "SELECT kind, data FROM objects WHERE repo = $1 AND oid = $2",
                &[&name, &query_oid],
            )
        })?;
        match rows.first() {
            Some(row) => {
                let kind: i32 = row.get(0);
                let data: Vec<u8> = row.get(1);
                let kind = GitKind::from_number(kind as i64)
                    .ok_or_else(|| format!("对象 {oid} 的类型编号 {kind} 无效"))?;
                Ok(Some(GitObject { kind, data }))
            }
            None => Ok(None),
        }
    }

    pub fn has_object(&self, repo: &str, oid: &str) -> Result<bool, String> {
        Ok(self.get_object(repo, oid)?.is_some())
    }

    pub fn list_refs(&self, repo: &str) -> Result<Vec<(String, String)>, String> {
        let name = repo.strip_suffix(".git").unwrap_or(repo);
        if let Some(guard) = self.db.sqlite() {
            let mut statement = guard
                .prepare("SELECT name, oid FROM refs WHERE repo = ?1 ORDER BY name")
                .map_err(|e| sqlite_error("查询引用失败", e))?;
            let rows = statement
                .query_map([name], |row| Ok((row.get(0)?, row.get(1)?)))
                .map_err(|e| sqlite_error("查询引用失败", e))?;
            let mut result = Vec::new();
            for row in rows {
                result.push(row.map_err(|e| sqlite_error("读取引用失败", e))?);
            }
            return Ok(result);
        }
        let name = name.to_string();
        let rows = self.db.with_postgres(move |client| {
            client.query(
                "SELECT name, oid FROM refs WHERE repo = $1 ORDER BY name",
                &[&name],
            )
        })?;
        Ok(rows.iter().map(|row| (row.get(0), row.get(1))).collect())
    }

    pub fn get_ref(&self, repo: &str, ref_name: &str) -> Result<Option<String>, String> {
        let name = repo.strip_suffix(".git").unwrap_or(repo);
        if let Some(guard) = self.db.sqlite() {
            return guard
                .query_row(
                    "SELECT oid FROM refs WHERE repo = ?1 AND name = ?2",
                    params![name, ref_name],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|e| sqlite_error("读取引用失败", e));
        }
        let name = name.to_string();
        let ref_name = ref_name.to_string();
        let rows = self.db.with_postgres(move |client| {
            client.query(
                "SELECT oid FROM refs WHERE repo = $1 AND name = $2",
                &[&name, &ref_name],
            )
        })?;
        Ok(rows.first().map(|row| row.get(0)))
    }

    pub fn update_ref(
        &self,
        repo: &str,
        ref_name: &str,
        expected_old: Option<&str>,
        new_oid: Option<&str>,
    ) -> Result<bool, String> {
        validate_ref_name(ref_name)?;
        let name = repo.strip_suffix(".git").unwrap_or(repo);
        if let Some(mut guard) = self.db.sqlite() {
            let tx = guard
                .transaction()
                .map_err(|e| sqlite_error("开启事务失败", e))?;
            let changed = match (new_oid, expected_old) {
                (Some(new_oid), Some(old)) => tx
                    .execute(
                        "UPDATE refs SET oid = ?3 WHERE repo = ?1 AND name = ?2 AND oid = ?4",
                        params![name, ref_name, new_oid, old],
                    )
                    .map_err(|e| sqlite_error("更新引用失败", e))?,
                (Some(new_oid), None) => tx
                    .execute(
                        "INSERT INTO refs(repo, name, oid) VALUES (?1, ?2, ?3) ON CONFLICT(repo, name) DO NOTHING",
                        params![name, ref_name, new_oid],
                    )
                    .map_err(|e| sqlite_error("创建引用失败", e))?,
                (None, Some(old)) => tx
                    .execute(
                        "DELETE FROM refs WHERE repo = ?1 AND name = ?2 AND oid = ?3",
                        params![name, ref_name, old],
                    )
                    .map_err(|e| sqlite_error("删除引用失败", e))?,
                (None, None) => tx
                    .execute("DELETE FROM refs WHERE repo = ?1 AND name = ?2", params![name, ref_name])
                    .map_err(|e| sqlite_error("删除引用失败", e))?,
            };
            tx.commit().map_err(|e| sqlite_error("提交事务失败", e))?;
            return Ok(changed == 1);
        }

        let name = name.to_string();
        let ref_name = ref_name.to_string();
        let new_oid = new_oid.map(str::to_string);
        let expected_old = expected_old.map(str::to_string);
        self.db.with_postgres(move |client| {
            let mut tx = client.transaction()?;
            let changed = match (new_oid.as_deref(), expected_old.as_deref()) {
                (Some(new_oid), Some(old)) => tx.execute(
                    "UPDATE refs SET oid = $3 WHERE repo = $1 AND name = $2 AND oid = $4",
                    &[&name, &ref_name, &new_oid, &old],
                )?,
                (Some(new_oid), None) => tx.execute(
                    "INSERT INTO refs(repo, name, oid) VALUES ($1, $2, $3) ON CONFLICT(repo, name) DO NOTHING",
                    &[&name, &ref_name, &new_oid],
                )?,
                (None, Some(old)) => tx.execute(
                    "DELETE FROM refs WHERE repo = $1 AND name = $2 AND oid = $3",
                    &[&name, &ref_name, &old],
                )?,
                (None, None) => tx.execute(
                    "DELETE FROM refs WHERE repo = $1 AND name = $2",
                    &[&name, &ref_name],
                )?,
            };
            tx.commit()?;
            Ok(changed == 1)
        })
    }

    pub fn get_setting(&self, key: &str) -> Result<Option<Vec<u8>>, String> {
        if let Some(guard) = self.db.sqlite() {
            return guard
                .query_row("SELECT value FROM settings WHERE key = ?1", [key], |row| {
                    row.get(0)
                })
                .optional()
                .map_err(|e| sqlite_error("读取设置失败", e));
        }
        let key = key.to_string();
        let rows = self.db.with_postgres(move |client| {
            client.query("SELECT value FROM settings WHERE key = $1", &[&key])
        })?;
        Ok(rows.first().map(|row| row.get(0)))
    }

    pub fn put_setting(&self, key: &str, value: &[u8]) -> Result<(), String> {
        if let Some(guard) = self.db.sqlite() {
            guard
                .execute(
                    "INSERT INTO settings(key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                    params![key, value],
                )
                .map_err(|e| sqlite_error("写入设置失败", e))?;
            return Ok(());
        }
        let key = key.to_string();
        let value = value.to_vec();
        self.db.with_postgres(move |client| {
            client.execute(
                "INSERT INTO settings(key, value) VALUES ($1, $2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                &[&key, &value],
            )
        })?;
        Ok(())
    }
}

pub fn validate_repo_name(repo: &str) -> Result<(), String> {
    let name = repo.strip_suffix(".git").unwrap_or(repo);
    if name.is_empty() || name.len() > 100 {
        return Err("仓库名为空或过长".to_string());
    }
    if name.starts_with('.') || name == "." || name == ".." {
        return Err("仓库名不能以点开头".to_string());
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    {
        return Err("仓库名只允许字母、数字、点、下划线和连字符".to_string());
    }
    Ok(())
}

pub fn validate_ref_name(name: &str) -> Result<(), String> {
    if !name.starts_with("refs/") {
        return Err("引用名必须以 refs/ 开头".to_string());
    }
    if name.ends_with('/') || name.ends_with(".lock") || name.contains("..") || name.contains("//")
    {
        return Err("引用名包含非法片段".to_string());
    }
    for segment in name.split('/') {
        if segment.is_empty()
            || segment.starts_with('.')
            || segment.ends_with('.')
            || segment == "@"
        {
            return Err("引用名包含非法片段".to_string());
        }
    }
    if name
        .chars()
        .any(|c| c.is_control() || matches!(c, ' ' | '~' | '^' | ':' | '?' | '*' | '[' | '\\'))
    {
        return Err("引用名包含非法字符".to_string());
    }
    Ok(())
}

fn normalize_repo_name(repo: &str) -> Result<String, String> {
    validate_repo_name(repo)?;
    Ok(repo.strip_suffix(".git").unwrap_or(repo).to_string())
}

fn update_repo_sqlite(
    connection: &mut rusqlite::Connection,
    old: &str,
    rename: Option<&str>,
) -> rusqlite::Result<UpdateOutcome> {
    let tx = connection.transaction()?;
    let exists: Option<i64> = tx
        .query_row("SELECT 1 FROM repos WHERE name = ?1", [old], |row| {
            row.get(0)
        })
        .optional()?;
    if exists.is_none() {
        return Ok(UpdateOutcome::NotFound);
    }
    let Some(new_name) = rename else {
        return Ok(UpdateOutcome::Updated);
    };
    let conflict: Option<i64> = tx
        .query_row("SELECT 1 FROM repos WHERE name = ?1", [new_name], |row| {
            row.get(0)
        })
        .optional()?;
    if conflict.is_some() {
        return Ok(UpdateOutcome::Conflict);
    }
    tx.execute(
        "UPDATE refs SET repo = ?1 WHERE repo = ?2",
        params![new_name, old],
    )?;
    tx.execute(
        "UPDATE objects SET repo = ?1 WHERE repo = ?2",
        params![new_name, old],
    )?;
    tx.execute(
        "UPDATE repos SET name = ?1 WHERE name = ?2",
        params![new_name, old],
    )?;
    tx.commit()?;
    Ok(UpdateOutcome::Updated)
}

fn update_repo_postgres(
    client: &mut postgres::Client,
    old: &str,
    rename: Option<&str>,
) -> Result<UpdateOutcome, postgres::Error> {
    let mut tx = client.transaction()?;
    if tx
        .query_opt("SELECT 1 FROM repos WHERE name = $1", &[&old])?
        .is_none()
    {
        return Ok(UpdateOutcome::NotFound);
    }
    let Some(new_name) = rename else {
        return Ok(UpdateOutcome::Updated);
    };
    if tx
        .query_opt("SELECT 1 FROM repos WHERE name = $1", &[&new_name])?
        .is_some()
    {
        return Ok(UpdateOutcome::Conflict);
    }
    tx.execute(
        "UPDATE refs SET repo = $1 WHERE repo = $2",
        &[&new_name, &old],
    )?;
    tx.execute(
        "UPDATE objects SET repo = $1 WHERE repo = $2",
        &[&new_name, &old],
    )?;
    tx.execute(
        "UPDATE repos SET name = $1 WHERE name = $2",
        &[&new_name, &old],
    )?;
    tx.commit()?;
    Ok(UpdateOutcome::Updated)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sqlite_store() -> (Store, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("git.db");
        let store = Store::open(path.to_str().unwrap()).unwrap();
        (store, dir)
    }

    fn sample_object() -> (GitKind, Vec<u8>) {
        (
            GitKind::Blob,
            b"a\x00b\r\n\xe4\xb8\xad\xe6\x96\x87".to_vec(),
        )
    }

    #[test]
    fn object_roundtrip_preserves_raw_bytes() {
        let (store, _dir) = sqlite_store();
        store.ensure_repo("demo").unwrap();
        let (kind, data) = sample_object();
        store
            .put_object(
                "demo",
                "0123456789abcdef0123456789abcdef01234567",
                kind,
                &data,
            )
            .unwrap();
        let loaded = store
            .get_object("demo", "0123456789abcdef0123456789abcdef01234567")
            .unwrap()
            .expect("对象应存在");
        assert_eq!(loaded.kind, kind);
        assert_eq!(loaded.data, data);
        assert!(store
            .has_object("demo", "0123456789abcdef0123456789abcdef01234567")
            .unwrap());
        assert!(!store
            .has_object("demo", "ffffffffffffffffffffffffffffffffffffffff")
            .unwrap());
    }

    #[test]
    fn duplicate_object_write_is_idempotent() {
        let (store, _dir) = sqlite_store();
        store.ensure_repo("demo").unwrap();
        let (kind, data) = sample_object();
        store
            .put_object(
                "demo",
                "0123456789abcdef0123456789abcdef01234567",
                kind,
                &data,
            )
            .unwrap();
        store
            .put_object(
                "demo",
                "0123456789abcdef0123456789abcdef01234567",
                kind,
                &data,
            )
            .unwrap();
        let refs: Vec<(String, String)> = store.list_refs("demo").unwrap();
        assert!(refs.is_empty());
    }

    #[test]
    fn ref_cas_rejects_stale_old_value() {
        let (store, _dir) = sqlite_store();
        store.ensure_repo("demo").unwrap();
        assert!(store
            .update_ref("demo", "refs/heads/main", None, Some("aaaa"))
            .unwrap());
        assert!(!store
            .update_ref("demo", "refs/heads/main", Some("bbbb"), Some("cccc"))
            .unwrap());
        assert_eq!(
            store.get_ref("demo", "refs/heads/main").unwrap().as_deref(),
            Some("aaaa")
        );
        assert!(store
            .update_ref("demo", "refs/heads/main", Some("aaaa"), Some("cccc"))
            .unwrap());
        assert_eq!(
            store.get_ref("demo", "refs/heads/main").unwrap().as_deref(),
            Some("cccc")
        );
    }

    #[test]
    fn ref_delete_requires_matching_old_value() {
        let (store, _dir) = sqlite_store();
        store.ensure_repo("demo").unwrap();
        store
            .update_ref("demo", "refs/heads/topic", None, Some("aaaa"))
            .unwrap();
        assert!(!store
            .update_ref("demo", "refs/heads/topic", Some("bbbb"), None)
            .unwrap());
        assert!(store.get_ref("demo", "refs/heads/topic").unwrap().is_some());
        assert!(store
            .update_ref("demo", "refs/heads/topic", Some("aaaa"), None)
            .unwrap());
        assert!(store.get_ref("demo", "refs/heads/topic").unwrap().is_none());
    }

    #[test]
    fn refs_are_listed_sorted_by_name() {
        let (store, _dir) = sqlite_store();
        store.ensure_repo("demo").unwrap();
        store
            .update_ref("demo", "refs/heads/zeta", None, Some("1111"))
            .unwrap();
        store
            .update_ref("demo", "refs/heads/alpha", None, Some("2222"))
            .unwrap();
        store
            .update_ref("demo", "refs/tags/v1", None, Some("3333"))
            .unwrap();
        let refs = store.list_refs("demo").unwrap();
        assert_eq!(
            refs,
            vec![
                ("refs/heads/alpha".to_string(), "2222".to_string()),
                ("refs/heads/zeta".to_string(), "1111".to_string()),
                ("refs/tags/v1".to_string(), "3333".to_string()),
            ]
        );
    }

    #[test]
    fn invalid_ref_names_are_rejected() {
        for name in [
            "HEAD",
            "refs/heads/",
            "refs/heads/a..b",
            "refs/heads/a b",
            "refs//x",
            "refs/heads/.hidden",
        ] {
            assert!(validate_ref_name(name).is_err(), "{name} 应被拒绝");
        }
        for name in ["refs/heads/main", "refs/tags/v1.0", "refs/heads/feature/x"] {
            assert!(validate_ref_name(name).is_ok(), "{name} 应被接受");
        }
    }

    #[test]
    fn repo_names_are_restricted_to_one_safe_segment() {
        for name in ["demo", "demo.git", "team-notes", "a_b.c"] {
            assert!(validate_repo_name(name).is_ok(), "{name} 应被接受");
        }
        for name in ["", "..", ".git", "a/b", "a b", "na@me"] {
            assert!(validate_repo_name(name).is_err(), "{name} 应被拒绝");
        }
    }

    #[test]
    fn settings_roundtrip_and_overwrite() {
        let (store, _dir) = sqlite_store();
        assert!(store.get_setting("ssh_host_key").unwrap().is_none());
        store.put_setting("ssh_host_key", b"first").unwrap();
        store.put_setting("ssh_host_key", b"second").unwrap();
        assert_eq!(
            store.get_setting("ssh_host_key").unwrap().as_deref(),
            Some(b"second".as_slice())
        );
    }

    #[test]
    fn repo_lifecycle_crud_cascades_and_validates() {
        let (store, _dir) = sqlite_store();
        assert!(store.create_repo("demo").unwrap());
        assert!(
            !store.create_repo("demo.git").unwrap(),
            "重复创建应返回 false"
        );
        assert_eq!(store.list_repos().unwrap().len(), 1);
        let info = store.get_repo("demo.git").unwrap().unwrap();
        assert_eq!(info.name, "demo");
        assert!(!info.created_at.is_empty());

        store
            .update_ref("demo", "refs/heads/main", None, Some("aaaa"))
            .unwrap();
        store
            .put_object("demo", "aaaa", GitKind::Blob, b"x")
            .unwrap();
        store.update_repo("demo", Some("renamed")).unwrap();
        assert!(store.get_repo("demo").unwrap().is_none());
        assert_eq!(
            store
                .get_ref("renamed", "refs/heads/main")
                .unwrap()
                .as_deref(),
            Some("aaaa")
        );
        assert!(store.has_object("renamed", "aaaa").unwrap());

        store.create_repo("other").unwrap();
        assert_eq!(
            store.update_repo("renamed", Some("other")),
            Err(RepoUpdateError::Conflict)
        );

        assert!(store.delete_repo("renamed").unwrap());
        assert!(!store.delete_repo("renamed").unwrap());
        assert!(store.list_refs("renamed").unwrap().is_empty());
        assert!(!store.has_object("renamed", "aaaa").unwrap());
    }

    #[test]
    fn repo_crud_normalizes_dot_git_suffix() {
        let (store, _dir) = sqlite_store();
        assert!(store.create_repo("demo.git").unwrap());
        assert!(store.get_repo("demo.git").unwrap().is_some());
        assert!(store.delete_repo("demo.git").unwrap());
        assert!(store.get_repo("demo").unwrap().is_none());
    }

    fn postgres_store() -> Option<Store> {
        let url = std::env::var("TEST_PG_URL").ok()?;
        let store = Store::open(&url).unwrap();
        store
            .db
            .with_postgres(|client| client.batch_execute("TRUNCATE repos, objects, refs, settings"))
            .unwrap();
        Some(store)
    }

    #[test]
    fn postgres_matches_sqlite_behavior() {
        let Some(store) = postgres_store() else {
            eprintln!("跳过 PostgreSQL 用例：未设置 TEST_PG_URL");
            return;
        };
        store.ensure_repo("demo").unwrap();
        let (kind, data) = sample_object();
        store
            .put_object(
                "demo",
                "0123456789abcdef0123456789abcdef01234567",
                kind,
                &data,
            )
            .unwrap();
        assert_eq!(
            store
                .get_object("demo", "0123456789abcdef0123456789abcdef01234567")
                .unwrap(),
            Some(GitObject { kind, data })
        );
        assert!(store
            .update_ref("demo", "refs/heads/main", None, Some("aaaa"))
            .unwrap());
        assert!(!store
            .update_ref("demo", "refs/heads/main", Some("bbbb"), Some("cccc"))
            .unwrap());
        assert_eq!(
            store.get_ref("demo", "refs/heads/main").unwrap().as_deref(),
            Some("aaaa")
        );
        store.put_setting("k", b"v").unwrap();
        assert_eq!(
            store.get_setting("k").unwrap().as_deref(),
            Some(b"v".as_slice())
        );

        assert!(store.create_repo("other").unwrap());
        assert_eq!(
            store.update_repo("demo", Some("other")),
            Err(RepoUpdateError::Conflict)
        );
        store.update_repo("demo", Some("renamed")).unwrap();
        assert_eq!(
            store
                .get_ref("renamed", "refs/heads/main")
                .unwrap()
                .as_deref(),
            Some("aaaa")
        );
        assert!(store.get_repo("demo").unwrap().is_none());
        assert!(store.delete_repo("renamed").unwrap());
        assert!(store.get_repo("renamed").unwrap().is_none());
        assert!(store.list_refs("renamed").unwrap().is_empty());
    }
}
