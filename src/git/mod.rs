/**
 * [INPUT]: 依赖 oid、object、delta 三个子模块，依赖 store::GitKind 与 GitObject
 * [OUTPUT]: 导出 Git 对象哈希、提交树标签解析、可达性遍历与差量应用
 * [POS]: Git 数据模型的纯函数层；不接触数据库与传输，协议核心与打包层共用
 * [PROTOCOL]: 变更时更新此头部，然后检查 CLAUDE.md
 */
pub mod advertise;
pub mod delta;
pub mod fetch;
pub mod object;
pub mod oid;
pub mod pack;
pub mod push;
pub mod wire;
