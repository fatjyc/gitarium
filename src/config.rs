/**
 * [INPUT]: 读取环境变量 GIT_USER、GIT_PASSWORD、DATABASE_URL、GIT_HTTP_ADDR、GIT_SSH_ADDR
 * [OUTPUT]: 导出 Config 与 Config::from_env、Config::from_pairs
 * [POS]: 启动时的唯一配置入口；未设置的环境变量使用固定默认值，服务不读取其他配置来源
 * [PROTOCOL]: 变更时更新此头部，然后检查 CLAUDE.md
 */
use std::collections::HashMap;

pub const DEFAULT_GIT_USER: &str = "git";
pub const DEFAULT_GIT_PASSWORD: &str = "git123";
pub const DEFAULT_HTTP_ADDR: &str = "127.0.0.1:3000";
pub const DEFAULT_SSH_ADDR: &str = "127.0.0.1:2222";
pub const DEFAULT_DATABASE_URL: &str = "data/gitdb.sqlite3";
pub const DEFAULT_MAX_PUSH_BYTES: usize = 268_435_456;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    pub database_url: String,
    pub http_addr: String,
    pub ssh_addr: String,
    pub git_user: String,
    pub git_password: String,
    pub max_push_bytes: usize,
}

impl Config {
    pub fn from_env() -> Config {
        let pairs: Vec<(String, String)> = std::env::vars().collect();
        Config::from_pairs(pairs)
    }

    pub fn from_pairs<I, K, V>(pairs: I) -> Config
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        let mut map: HashMap<String, String> = pairs
            .into_iter()
            .map(|(key, value)| (key.into(), value.into()))
            .collect();
        Config {
            database_url: map
                .remove("DATABASE_URL")
                .unwrap_or_else(|| DEFAULT_DATABASE_URL.to_string()),
            http_addr: map
                .remove("GIT_HTTP_ADDR")
                .unwrap_or_else(|| DEFAULT_HTTP_ADDR.to_string()),
            ssh_addr: map
                .remove("GIT_SSH_ADDR")
                .unwrap_or_else(|| DEFAULT_SSH_ADDR.to_string()),
            git_user: map
                .remove("GIT_USER")
                .unwrap_or_else(|| DEFAULT_GIT_USER.to_string()),
            git_password: map
                .remove("GIT_PASSWORD")
                .unwrap_or_else(|| DEFAULT_GIT_PASSWORD.to_string()),
            max_push_bytes: map
                .remove("GIT_MAX_PUSH_BYTES")
                .and_then(|value| value.parse().ok())
                .unwrap_or(DEFAULT_MAX_PUSH_BYTES),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_variables_use_fixed_git_account() {
        let config = Config::from_pairs(Vec::<(String, String)>::new());
        assert_eq!(config.git_user, "git");
        assert_eq!(config.git_password, "git123");
        assert_eq!(config.http_addr, "127.0.0.1:3000");
        assert_eq!(config.ssh_addr, "127.0.0.1:2222");
        assert_eq!(config.database_url, "data/gitdb.sqlite3");
        assert_eq!(config.max_push_bytes, 268_435_456);
    }

    #[test]
    fn explicit_values_override_defaults() {
        let config = Config::from_pairs(vec![
            ("GIT_USER", "ci"),
            ("GIT_PASSWORD", "s3cret"),
            ("DATABASE_URL", "postgres://db/git"),
            ("GIT_HTTP_ADDR", "127.0.0.1:4000"),
            ("GIT_MAX_PUSH_BYTES", "1024"),
        ]);
        assert_eq!(config.git_user, "ci");
        assert_eq!(config.git_password, "s3cret");
        assert_eq!(config.database_url, "postgres://db/git");
        assert_eq!(config.http_addr, "127.0.0.1:4000");
        assert_eq!(config.ssh_addr, "127.0.0.1:2222");
        assert_eq!(config.max_push_bytes, 1024);
    }
}
