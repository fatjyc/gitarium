# src/
> L2 | 父级: ../CLAUDE.md

成员列表

main.rs: 可执行程序入口；在异步运行时之外打开数据库，再绑定 HTTP 与 SSH 监听并并发运行。
lib.rs: 库入口；声明 config、db、store、git、auth、api、http、ssh 八个模块。
config.rs: 解析启动环境变量（DATABASE_URL、GIT_HTTP_ADDR、GIT_SSH_ADDR、GIT_USER、GIT_PASSWORD、GIT_MAX_PUSH_BYTES），提供固定默认账号；不读取其他配置来源。
db.rs: 打开 SQLite（互斥单连接、WAL）或 PostgreSQL（互斥单连接，断线重连），并创建 repos、objects、refs、settings 四张表；迁移时删除旧库遗留的 default_branch 列；PostgreSQL 连接必须在异步运行时之外打开；上层的 SQL 分派在 store.rs。
store.rs: 唯一的业务数据接口；对象幂等写入与读取、引用比较并交换更新、设置读写、仓库名与引用名校验；仓库生命周期（列表、详情、创建、改名、删除）跨 repos/refs/objects 三表在单个事务内完成。
auth.rs: HTTP Basic 认证的唯一实现；解码固定账号凭据，数据面与管理 API 共用；不解析路由，不接触 Git 语义。
api.rs: JSON 管理 API；`/api/repos` 列表与创建、`/api/repos/{repo}` 详情、改名与删除；复用 http::App 状态与 Basic 认证，删除要求 confirm 与仓库名一致。
http.rs: HTTP 传输层；合并管理 API 路由，处理 Git 数据面的 info/refs 通告与 upload-pack/receive-pack 字节转发；拉取不存在的仓库返回 404。
ssh.rs: SSH 传输层；密码认证、git-upload-pack 与 git-receive-pack 命令白名单、Ed25519 主机密钥入库；拉取不存在的仓库直接失败；把通道字节桥接给同步协议核心。

[PROTOCOL]: 变更时更新此头部，然后检查 CLAUDE.md
