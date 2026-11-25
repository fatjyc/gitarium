# Additional Conventions Beyond the Built-in Functions

As this project's AI coding tool, you must follow the additional conventions below, in addition to the built-in functions.

# gitarium - 把 Git 仓库存进数据库的内部 Git 服务器

Rust 2021 + axum 0.8 + russh 0.65.4 + gix-pack 0.77.0 + rusqlite 0.43 + postgres 0.19

<directory>
src/ - 服务入口、配置、数据库访问、认证与 HTTP/SSH 传输（9 个文件：main、lib、config、db、store、auth、api、http、ssh）
src/git/ - Git 对象模型、pack、引用通告与推拉核心（9 个文件：mod、oid、object、delta、wire、advertise、pack、push、fetch）
tests/ - 真实 git 客户端驱动的端到端验收（5 个文件：common、e2e_api、e2e_http、e2e_ssh、e2e_migration）
scripts/ - PostgreSQL 迁移验收脚本
docs/superpowers/ - 设计文档与实施计划
</directory>
<config>
Cargo.toml - 依赖与版本；gix-pack 固定 0.77.0 以匹配 Rust 1.94
README.md - 启动、推拉、仓库管理、认证与迁移说明
LICENSE - MIT 许可证
</config>

Laws: 仓库数据只在数据库 · 只用真实 git 验收 · 固定账号 · 管理 API 同账号 · 协议 v0 · SQLite 与 PostgreSQL 行为一致
