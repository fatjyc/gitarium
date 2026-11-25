# 数据库 Git 服务器设计

2026-11-25。内部小规模使用，支持标准 Git 客户端通过 HTTPS 和 SSH 推拉。

## 目的

整个 Git 仓库（文件内容、提交历史、目录树、标签、分支指向）存进数据库。
换机器时恢复数据库并启动同一服务，克隆结果与原来一致，还能继续推拉。
开发者使用标准 `git` 客户端，不需要插件或新工作习惯。

## 目标与非目标

目标：

- HTTPS（Smart HTTP）与 SSH 两个入口，传输同一种仓库数据。
- 支持 `clone`、`fetch`、`pull`、`push`、分支与标签、删除远端分支。
- 首次推送自动创建仓库。
- JSON 管理 API：列出、创建、查看、改名、删除仓库。删除必须显式确认。
- SQLite 用于本机测试；PostgreSQL 用 Docker 启动，用于发布。两种后端行为一致。
- 单个固定账号密码，HTTPS 与 SSH 共用同一组凭据。
- 仓库数据只存在数据库中。进程只使用内存中的请求与响应。

非目标（第一版不做）：

- 网页界面、Pull Request、CI、代码搜索。
- 多用户、仓库权限、公钥认证。
- 按 pack 文件整体存储（保留差量压缩）。
- Git 协议 v2、浅克隆、部分克隆、大文件存储。

失败即不达标的情况：

- 需要同时备份数据库和某台机器上的仓库目录。
- 特殊 Git 客户端或改配置才能推拉。
- 两个客户端同时推送时，后到的请求悄悄覆盖先到的分支。

## 已排除的方案

- 复用 rgit-server 原样部署：其 SQLite 只存用户和仓库元数据，提交内容在磁盘裸仓库中
  （`repos/<owner>/<repo>.git`）。不满足迁移目标。
- 把客户端上传的 pack 文件整体存为一行：保留差量压缩，但读取时要维护 pack 索引与差量依赖。
- 每行存一个文件快照：无法表达提交历史与树结构，Git 协议无法直接复用。

## 选定方案

按 Git 对象存储：

- 每个 Git 对象一行：`(repo, oid, kind, data)`。`data` 为未压缩的原始对象内容。
  文本按原始字节保存，SQLite 用 BLOB，PostgreSQL 用 BYTEA。
- 引用单独存放：`(repo, ref_name, oid)`。
- 推送：解码 pack 包，解析差量得到完整对象，按 oid 幂等写入，再用比较并交换更新引用。
- 拉取：从客户端请求的引用出发遍历对象图，编码为不含差量的 pack 包返回。
- HEAD 是符号引用，固定指向 `refs/heads/main`。
- SSH 主机密钥也存入数据库，迁移后客户端不会看到主机密钥变化。

### 技术栈

Rust 1.94；tokio；axum（HTTP）；russh（SSH）；gix-pack（解码 pack 与校验包尾）；
rusqlite（SQLite，bundled）；postgres（PostgreSQL，单工作线程）；flate2（编码 pack）；sha1（对象 ID）；
base64（Basic 认证解析）；OpenSSH askpass（测试中给 SSH 客户端作答）。

### 数据库模式

```sql
repos(name TEXT PRIMARY KEY, created_at TEXT NOT NULL)
objects(repo TEXT NOT NULL, oid TEXT NOT NULL, kind INTEGER NOT NULL, data BLOB, PRIMARY KEY(repo, oid))
refs(repo TEXT NOT NULL, name TEXT NOT NULL, oid TEXT NOT NULL, PRIMARY KEY(repo, name))
settings(key TEXT PRIMARY KEY, value BLOB)
```

PostgreSQL 建表时 `BLOB` 写作 `BYTEA`。对象 `kind`：1 commit、2 tree、3 blob、4 tag。
`oid` 为 40 位小写十六进制（SHA-1）。

### 接口

- HTTP：`GET /{name}.git/info/refs?service=git-upload-pack|git-receive-pack`；
  `POST /{name}.git/git-upload-pack`；`POST /{name}.git/git-receive-pack`。
  Basic 认证，失败返回 401 与 `WWW-Authenticate: Basic realm="git"`。
- 管理 API：`GET|POST /api/repos`；`GET|PATCH|DELETE /api/repos/{repo}`。
  与 Git 数据面共用监听和固定账号。PATCH 接受 `new_name`；
  DELETE 请求体必须带 `{"confirm":"<仓库名>"}`。
- 仓库生命周期：推送自动建仓；拉取与克隆不存在的仓库直接失败；删除同时清除该仓库的引用与对象。
- SSH：用户 `git`（可配置），密码固定。接受且仅接受
  `git-upload-pack '<path>'` 与 `git-receive-pack '<path>'` 两种命令。
- 仓库名：单段名称，可带或不带 `.git` 后缀；只允许字母、数字、`.`、`_`、`-`。

### 协议范围

只实现 v0 协议。客户端请求 v2 时服务端仍返回 v0 通告，Git 客户端兼容。
拉取时忽略 `have` 行，始终发送 wants 可达的全部对象：传输量大于最优值，但语义正确，
内部文本仓库规模可接受。推送必须支持薄包：REF_DELTA 的基对象可能只在数据库中。

### 并发

引用更新使用比较并交换：旧值匹配才写入，否则返回 `ng`。SQLite 用单连接互斥；
PostgreSQL 用单工作线程串行执行 SQL 与条件更新语句。`postgres` 同步客户端会自建 Tokio 运行时，
在异步运行时内调用会 panic，因此连接与 SQL 都放在独立工作线程，Store 通过通道等待结果。

### 迁移验收

- 在同一数据库上重启服务：分支、标签、历史不变。
- 复制 SQLite 数据文件到新目录启动新实例：克隆出的 `git rev-parse HEAD`、
  `git tag`、`git fsck` 结果一致。
- PostgreSQL：`pg_dump` 导出并恢复到第二个数据库，新实例克隆结果一致。
- SQLite 数据迁到 PostgreSQL：通过 Git 镜像推送搬运分支与标签。

### 认证边界

单一固定账号密码，来自环境变量 `GIT_USER`（默认 `git`）与 `GIT_PASSWORD`
（默认 `git123`）。没有账号管理界面。两个协议与管理 API 都校验同一组值。
管理 API 的能力等同于仓库管理员：任何持有该账号的人都可以创建、改名和删除仓库。

## 交给实现者的决定

- 具体 crate 的小版本、SQL 语句细节、pack 编码缓冲策略。
- 错误信息文案（除协议要求的状态行外）。
- 测试中临时目录、端口分配方式。
