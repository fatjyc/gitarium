/**
 * [INPUT]: 依赖 config 与 db 模块
 * [OUTPUT]: 导出 Config、Db 与 Store，供传输层与协议核心使用
 * [POS]: crate 根，负责模块声明，不承载业务逻辑
 * [PROTOCOL]: 变更时更新此头部，然后检查 CLAUDE.md
 */
pub mod config;
pub mod db;
pub mod git;
pub mod http;
pub mod ssh;
pub mod store;
