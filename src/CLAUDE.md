# src/
> L2 | 父级: ../CLAUDE.md

成员列表

main.rs: 可执行程序入口；在异步运行时之外打开数据库，再绑定 HTTP 与 SSH 监听并并发运行。
lib.rs: 库入口；声明 config、db、store、git、http、ssh 六个模块。
config.rs: 解析启动环境变量（DATABASE_URL、GIT_HTTP_ADDR、GIT_SSH_ADDR、GIT_USER、GIT_PASSWORD、GIT_MAX_PUSH_BYTES），提供固定默认账号；不读取其他配置来源。
db.rs: 打开 SQLite（互斥单连接、WAL）或 PostgreSQL（互斥单连接，断线重连），并创建 repos、objects、refs、settings 四张表；PostgreSQL 连接必须在异步运行时之外打开；上层的 SQL 分派在 store.rs。
store.rs: 唯一的业务数据接口；对象幂等写入与读取、引用比较并交换更新、设置读写、仓库名与引用名校验；SQLite 与 PostgreSQL 各一套 SQL。
http.rs: HTTP 传输层；Basic 认证、仓库路径解析、info/refs 通告与 upload-pack/receive-pack 字节转发；只做传输，不承载 Git 语义。
ssh.rs: SSH 传输层；密码认证、git-upload-pack 与 git-receive-pack 命令白名单、Ed25519 主机密钥入库；把通道字节桥接给同步协议核心。

[PROTOCOL]: 变更时更新此头部，然后检查 CLAUDE.md
