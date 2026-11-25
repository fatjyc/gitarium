# gitarium

把整个 Git 仓库存进数据库的内部 Git 服务器。标准 `git` 客户端通过 Smart HTTP 与 SSH 推拉；
对象、引用、仓库元数据和 SSH 主机密钥全部保存在 SQLite 或 PostgreSQL 中，迁移时恢复数据库即可。

## 能力与边界

- 支持 `clone`、`fetch`、`pull`、`push`、分支与标签，使用 Git 协议 v0。
- HTTP 走 Smart HTTP；SSH 只接受 `git-upload-pack` 与 `git-receive-pack` 两条命令。
- 单一固定账号，HTTP Basic、管理 API 与 SSH 密码认证使用同一组值；没有账号管理。
- 仓库名为单段名称，可带或不带 `.git`；第一次推送自动建仓，也可以用管理 API 预先创建。
- 默认分支固定为 `main`：HEAD 指向 `refs/heads/main`，首次推送请推送到 `main`，克隆的默认检出依赖它。
- 克隆不存在的仓库会失败；删除仓库后，推拉与克隆同样失败。
- 拉取忽略 `have` 协商，总是发送 wants 可达的全部对象；内部文本仓库规模可接受。
- SSH 主机密钥首次启动时生成，保存在 `settings` 表；恢复数据库后客户端不会看到密钥变化。
- 服务本身监听明文 HTTP。需要 HTTPS 时，在前置反向代理终止 TLS；Basic 凭据不要直接暴露在不可信网络。

## 启动

SQLite（单机测试）：

```bash
DATABASE_URL=data/gitarium.sqlite3 \
GIT_HTTP_ADDR=127.0.0.1:3000 \
GIT_SSH_ADDR=127.0.0.1:2222 \
cargo run
```

PostgreSQL（发布）：

```bash
docker run -d --name gitarium-pg \
  -e POSTGRES_USER=gitarium -e POSTGRES_PASSWORD=gitarium \
  -p 127.0.0.1:5432:5432 postgres:17-alpine
docker exec gitarium-pg createdb -U gitarium gitarium

DATABASE_URL=postgres://gitarium:gitarium@127.0.0.1:5432/gitarium \
GIT_HTTP_ADDR=0.0.0.0:3000 \
GIT_SSH_ADDR=0.0.0.0:2222 \
cargo run
```

数据库地址以 `postgres://` 或 `postgresql://` 开头时使用 PostgreSQL，其余按 SQLite 文件路径处理。

## 环境变量

| 变量 | 默认值 | 说明 |
|------|--------|------|
| `DATABASE_URL` | `data/gitarium.sqlite3` | `postgres://...` 走 PostgreSQL，其余视为 SQLite 文件路径 |
| `GIT_HTTP_ADDR` | `127.0.0.1:3000` | HTTP 监听地址 |
| `GIT_SSH_ADDR` | `127.0.0.1:2222` | SSH 监听地址 |
| `GIT_USER` | `git` | 推拉用户名 |
| `GIT_PASSWORD` | `git123` | 推拉密码 |
| `GIT_MAX_PUSH_BYTES` | `268435456` | 单个推送包与拉取对象总量的上限（字节） |

## 推拉

```bash
git remote add origin http://git:git123@127.0.0.1:3000/demo.git
git push -u origin main --tags

git clone http://git:git123@127.0.0.1:3000/demo.git
git clone ssh://git@127.0.0.1:2222/demo.git
```

SSH 会提示输入密码（默认 `git123`）。仓库 URL 中的 `.git` 后缀可以省略。

## 仓库管理

管理接口与 Git 推拉共用同一个 HTTP 监听和同一组 Basic 账号，请求与响应都是 JSON。

```bash
AUTH='git:git123'

# 列出全部仓库
curl -u "$AUTH" http://127.0.0.1:3000/api/repos

# 预先创建仓库（已存在返回 409）
curl -u "$AUTH" -X POST http://127.0.0.1:3000/api/repos \
  -H 'Content-Type: application/json' -d '{"name":"demo"}'

# 查看仓库详情与全部引用
curl -u "$AUTH" http://127.0.0.1:3000/api/repos/demo

# 改名
curl -u "$AUTH" -X PATCH http://127.0.0.1:3000/api/repos/demo \
  -H 'Content-Type: application/json' -d '{"new_name":"team-notes"}'

# 删除仓库：confirm 必须与仓库名完全一致，否则返回 400
curl -u "$AUTH" -X DELETE http://127.0.0.1:3000/api/repos/team-notes \
  -H 'Content-Type: application/json' -d '{"confirm":"team-notes"}'
```

接口一览：

| 方法与路径 | 作用 | 状态码 |
|------------|------|--------|
| `GET /api/repos` | 仓库列表 | 200 |
| `POST /api/repos` | 创建仓库，请求体 `{"name":"..."}` | 201、409 |
| `GET /api/repos/{repo}` | 详情与引用 | 200、404 |
| `PATCH /api/repos/{repo}` | 改名，请求体 `{"new_name":"..."}` | 200、400、404、409 |
| `DELETE /api/repos/{repo}` | 删除仓库，请求体 `{"confirm":"..."}` | 204、400、404 |

仓库改名与删除在一个数据库事务中同时更新引用表和对象表，不会留下半个仓库。

## AI 规则

`.rulesync/` 是规则源，根目录 `CLAUDE.md` 与 `AGENTS.md` 由 rulesync 生成，不要直接改。
修改规则时编辑 `.rulesync/rules/`，然后运行：

```bash
make rules
```

当前配置生成 Claude Code 的 `CLAUDE.md` 与通用 `AGENTS.md`；目标与特性在 `rulesync.jsonc` 中调整。
Makefile 默认使用 `npx --yes rulesync@43.14.0`，可用 `RULESYNC=...` 覆盖。

## 迁移

SQLite：

1. 停止服务，等待 `*.sqlite3-wal` 消失（最后一个连接关闭时 SQLite 会做检查点）。
2. 复制 `*.sqlite3` 到新机器。
3. 新机器用同一个 `DATABASE_URL` 指向副本启动。

PostgreSQL：

1. `pg_dump` 导出数据库。
2. 恢复到新数据库。
3. 新实例的 `DATABASE_URL` 指向恢复后的数据库。

两种方式都保留分支、标签、历史与 SSH 主机密钥。跨后端迁移用 Git 镜像搬运：

```bash
git clone --mirror http://git:git123@old:3000/demo.git demo.git
git -C demo.git push --mirror http://git:git123@new:3000/demo.git
```

## 测试与验收

```bash
cargo test
./scripts/acceptance_pg.sh
```

`cargo test` 使用真实 `git` 与 `ssh` 客户端，覆盖 HTTP/SSH 互操作、首次推送建仓、标签、二进制字节、
过期推送拒绝、管理 API 增删改查、SQLite 文件复制恢复与同一数据库重启。

`scripts/acceptance_pg.sh` 需要 Docker。脚本启动隔离的 PostgreSQL 容器，验证 SQLite → PostgreSQL
镜像迁移、PostgreSQL 后端测试、`pg_dump` 恢复、恢复后继续推送与 SSH 克隆，结束后自动清理容器。

## 设计

- 设计：`docs/superpowers/specs/2026-11-25-git-db-server-design.md`
- 实施计划：`docs/superpowers/plans/2026-11-25-git-db-server.md`

## 许可证

[MIT](LICENSE)
