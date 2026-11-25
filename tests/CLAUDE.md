# tests/
> L2 | 父级: ../CLAUDE.md

成员列表

common/mod.rs: 集成测试装置；每个测试独立临时目录、SQLite 数据库与随机端口，同时启动 HTTP 与 SSH；提供 git/ssh 命令与 askpass 辅助。
e2e_http.rs: HTTP 端到端；认证、首次推送建仓、克隆、拉取、标签、二进制字节、过期推送拒绝与空仓库克隆。
e2e_ssh.rs: SSH 端到端；密码认证、命令白名单、SQLite 主机密钥重启不变，以及与 HTTP 的双向推拉互操作。
e2e_migration.rs: SQLite 迁移验收；停止服务后复制数据库文件，恢复历史、标签与 fsck，继续推送并在同一数据库重启。

[PROTOCOL]: 变更时更新此头部，然后检查 CLAUDE.md
