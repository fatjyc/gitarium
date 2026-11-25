# 数据库 Git 服务器实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 用 Rust 实现一个 Git 服务器，仓库数据全部存进 SQLite 或 PostgreSQL，标准 Git 客户端可通过 HTTPS 和 SSH 推拉。

**Architecture:** 传输层（axum HTTP、russh SSH）只做认证与字节转发；共用的协议核心同步处理 pkt-line、pack、对象与引用，数据库同步访问层对两种后端提供同一组操作。仓库每次只从数据库读取，不落盘。

**Tech Stack:** Rust 1.94, tokio, axum 0.8, russh 0.65, gix-pack 0.78, rusqlite (bundled), postgres + r2d2, flate2, sha1, base64.

**Spec:** `docs/superpowers/specs/2026-11-25-git-db-server-design.md`

## Global Constraints

- 只使用标准 `git` 客户端可观察的行为做验收；测试必须调用真实 `git` 可执行文件。
- 对象内容按原始字节保存与还原；测试必须覆盖中文、CRLF、NUL 与无结尾换行的文本。
- 仓库数据只存数据库；进程内允许内存缓冲，禁止使用裸仓库目录。
- 认证为单一固定账号密码，来自 `GIT_USER`（默认 `git`）与 `GIT_PASSWORD`（默认 `git123`）。
- 只实现 Git 协议 v0；不广告 side-band、multi_ack、shallow、push-options、atomic。
- SQLite 与 PostgreSQL 必须通过同一组行为测试。
- 每个源码文件带中文 L3 头（[INPUT]/[OUTPUT]/[POS]/[PROTOCOL]）；模块目录带 L2 `CLAUDE.md`；项目根最后写 L1 `CLAUDE.md`。
- 未获得用户明确授权前不执行 `git init` 或提交；步骤中的提交动作默认省略。

## Review Focus

- 空仓库的引用通告与首次 `git push`：客户端收到零 ID 能力行后仍能推送成功。
- 两个客户端同时更新同一分支：先到者成功，后到者收到 `ng` 而不是覆盖。
- 薄包 REF_DELTA 的基对象只在数据库中：推送后所有对象可被克隆还原。
- 注释标签：克隆与 `git fsck` 结果一致，`git tag` 能列出。
- 非 UTF-8 或含 NUL 的文本、无结尾换行：克隆后字节完全一致。
- 仓库名带 `.git` 后缀、URL 带凭据、错误密码：分别成功或返回 401。

---

### Task 1: 项目骨架、配置与数据库初始化

**Files:**
- Create: `Cargo.toml`, `.gitignore`, `README.md`
- Create: `src/main.rs`, `src/lib.rs`, `src/config.rs`, `src/db.rs`
- Create: `src/CLAUDE.md`
- Test: `src/config.rs` 内联测试，`src/db.rs` 内联测试

**Interfaces:**
- `Config::from_env() -> Config`，字段 `database_url: String`、`http_addr: String`、`ssh_addr: String`、`git_user: String`、`git_password: String`。
- `Db::open(url: &str) -> Result<Db, String>`；`Db` 为 `enum Db { Sqlite(Mutex<rusqlite::Connection>), Postgres(r2d2::Pool<PostgresConnectionManager<NoTls>>) }`。
- `Db::migrate(&self) -> Result<(), String>` 创建四张表（`repos`、`objects`、`refs`、`settings`）。

- [ ] **Step 1: 写失败测试**：`config_defaults_use_fixed_git_account`（清空 `GIT_USER`/`GIT_PASSWORD` 后得到 `git`/`git123`）；`migrate_creates_all_tables`（临时 SQLite 文件，查询四张表存在）。
- [ ] **Step 2: 运行确认失败**：`cargo test`，预期编译失败或断言失败。
- [ ] **Step 3: 实现**：Cargo 依赖与上述接口；SQLite 建表语句用 `BLOB`，PostgreSQL 用 `BYTEA`；SQLite 打开后执行 `PRAGMA journal_mode=WAL; PRAGMA busy_timeout=5000;`。
- [ ] **Step 4: 运行确认通过**：`cargo test`。
- [ ] **Step 5: 写 README 最小用法**：环境变量表与 `cargo run` 命令。

### Task 2: 对象、引用与设置的存储

**Files:**
- Create: `src/store.rs`（或 `src/store/mod.rs`）
- Test: 内联测试（SQLite）；PostgreSQL 测试在有 `TEST_PG_URL` 时运行，否则跳过并在输出中说明

**Interfaces:**
- `pub enum GitKind { Commit=1, Tree=2, Blob=3, Tag=4 }`，`GitKind::from_pack_type(u8) -> Option<GitKind>`。
- `pub struct GitObject { pub kind: GitKind, pub data: Vec<u8> }`。
- `pub struct Store { db: Db }`，方法：
  - `ensure_repo(&self, repo: &str) -> Result<(), String>`
  - `repo_exists(&self, repo: &str) -> Result<bool, String>`
  - `put_object(&self, repo: &str, oid: &str, kind: GitKind, data: &[u8]) -> Result<(), String>`（幂等）
  - `get_object(&self, repo: &str, oid: &str) -> Result<Option<GitObject>, String>`
  - `has_object(&self, repo: &str, oid: &str) -> Result<bool, String>`
  - `list_refs(&self, repo: &str) -> Result<Vec<(String, String)>, String>`（按名称排序）
  - `get_ref(&self, repo: &str, name: &str) -> Result<Option<String>, String>`
  - `update_ref(&self, repo: &str, name: &str, expected_old: Option<&str>, new_oid: Option<&str>) -> Result<bool, String>`：比较并交换，成功返回 `true`；`new_oid=None` 表示删除；`expected_old=None` 表示要求不存在。
  - `get_setting(&self, key: &str) -> Result<Option<Vec<u8>>, String>`、`put_setting(&self, key: &str, value: &[u8]) -> Result<(), String>`
- 引用名称校验：必须 `refs/` 开头；拒绝 `HEAD`、空段、`..`、控制字符、结尾 `/`、`.lock`。

- [ ] **Step 1: 写失败测试**：对象字节回环（`b"a\x00b\r\n中文"` 与长度、kind 一致）；重复写入不报错；引用创建成功后 `get_ref` 可见；用错误旧值更新返回 `false` 且值不变；删除成功后不可见；非法引用名被拒。
- [ ] **Step 2: 运行确认失败**：`cargo test store`。
- [ ] **Step 3: 实现**：SQLite 与 PostgreSQL 各一套 SQL；PostgreSQL 用条件语句实现 CAS（`UPDATE ... WHERE oid=$3` 或 `INSERT ... ON CONFLICT DO NOTHING`），SQLite 用事务 + `changes()`。
- [ ] **Step 4: 运行确认通过**：`cargo test store`；若设置 `TEST_PG_URL`，再运行 `TEST_PG_URL=... cargo test store`，两个后端都通过。

### Task 3: Git 对象模型、可达性与差量

**Files:**
- Create: `src/git/mod.rs`, `src/git/oid.rs`, `src/git/object.rs`, `src/git/delta.rs`
- Create: `src/git/CLAUDE.md`
- Test: 内联单元测试

**Interfaces:**
- `pub fn object_oid(kind: GitKind, data: &[u8]) -> String`：`sha1("<type> <len>\0" + data)` 的小写十六进制。
- `pub fn parse_commit_tree(data: &[u8]) -> Result<String, String>`、`pub fn parse_commit_parents(data: &[u8]) -> Result<Vec<String>, String>`、`pub fn parse_tag_target(data: &[u8]) -> Result<(String, GitKind), String>`。
- `pub fn tree_entries(data: &[u8]) -> Result<Vec<(String /*oid*/, u32 /*mode*/, Vec<u8> /*name*/)>, String>`。
- `pub fn reachable<F>(wants: &[String], get: F) -> Result<Vec<(String, GitKind)>, String>`，`F: FnMut(&str) -> Result<Option<GitObject>, String>`；标签沿 `object` 走，提交沿 `tree` 与 `parent` 走，树沿非 gitlink 条目走；结果去重、按 oid 排序。
- `pub fn apply_delta(base: &[u8], delta: &[u8]) -> Result<Vec<u8>, String>`。

- [ ] **Step 1: 写失败测试**：用固定字面对象计算 oid，断言等于 `git hash-object -t tree --stdin` 类命令的手工值（在测试中调用真实 git 生成期望值，或使用已知空 blob `e69de29bb2d1d6434b8b29ae775ad8c2e48c5391`）；解析一个手工构造的提交与树；构造 commit→tree→blob 图，断言 `reachable` 顺序与集合；应用一段手工 delta 得到期望字节。
- [ ] **Step 2: 运行确认失败**。
- [ ] **Step 3: 实现**：SHA-1 用 `sha1` crate；树条目按 `<mode> <name>\0<20 字节>` 解析；delta 支持 copy/insert 指令与两种 varint；`reachable` 用显式栈避免递归深度问题。
- [ ] **Step 4: 运行确认通过**。

### Task 4: pkt-line、引用通告与解码 pack

**Files:**
- Create: `src/git/wire.rs`, `src/git/advertise.rs`, `src/git/pack.rs`
- Test: 内联单元测试 + 用真实 `git pack-objects` 生成夹具

**Interfaces:**
- `wire::write_pkt(out: &mut dyn Write, data: &[u8])`、`wire::write_flush(out)`、`wire::read_pkt(reader: &mut dyn BufRead) -> Result<Option<Vec<u8>>, String>`（`None` 表示 flush）、`wire::read_line_or_flush` 返回枚举 `Pkt::Flush | Pkt::Line(Vec<u8>) | Pkt::Eof`。
- `advertise::refs(store: &Store, repo: &str, service: Service) -> Result<Vec<u8>, String>`：返回不含 `# service=` 前缀的引用通告，首行附带能力；`HEAD` 行带 `symref=HEAD:refs/<default_branch>`；注释标签追加 `^{}` 行；无引用时使用零 ID 与 `capabilities^{}`。
- `Service::{UploadPack, ReceivePack}`，`Service::name() -> &'static str`。
- `pub struct Ingested { pub objects: Vec<(String, GitKind, Vec<u8>)> }`；`pack::decode(reader: &mut dyn BufRead, lookup: &dyn Fn(&str) -> Result<Option<GitObject>, String>, limit: usize) -> Result<Ingested, String>`：用 `gix_pack::data::input::BytesToEntriesIter`（`Mode::Verify`、`EntryDataMode::Keep`）迭代，解压非差量对象，按 pack offset 与 oid 两阶段解析 OFS/REF 差量，REF 差量基对象缺失时调用 `lookup` 从数据库取。
- `pack::encode(objects: &[(String, GitKind, Vec<u8>)], out: &mut dyn Write) -> Result<(), String>`：写 PACK v2，全量对象无差量，zlib 压缩，尾部 SHA-1。

- [ ] **Step 1: 写失败测试**：pkt-line 编码 `b"done\n"` 得 `0009done\n`、flush 得 `0000`；通告首行能力与 `symref` 正确；用 `git init` 建临时仓库、提交含中文的文件、`git pack-objects --stdout --all` 得到 pack，`pack::decode` 还原的对象集合与 `git cat-file --batch-all-objects` 一致；`pack::encode` 的输出交给 `git index-pack --stdin` 成功且对象 ID 集合一致；构造薄包：对同一仓库第二次改动后运行 `git pack-objects --thin --stdout --all`，把基提交对象放进数据库，`decode` 后仍能还原。
- [ ] **Step 2: 运行确认失败**。
- [ ] **Step 3: 实现**：压缩用 `flate2::read::ZlibDecoder`；`pack::encode` 头部类型与长度编码按 Git pack 格式；`limit` 限制总字节数。
- [ ] **Step 4: 运行确认通过**：`cargo test wire advertise pack`。

### Task 5: HTTP 入口、认证与推送

**Files:**
- Create: `src/http.rs`, `src/git/push.rs`
- Create: `src/CLAUDE.md`（补充 http）
- Test: `tests/e2e_http.rs`，`tests/common/mod.rs`（临时目录、端口、git 命令封装）

**Interfaces:**
- `push::receive(store: &Store, repo: &str, input: &mut dyn BufRead, out: &mut dyn Write, max: usize) -> Result<Vec<String> /*更新成功的引用*/, String>`：解析命令、校验引用名与对象存在性、解码 pack、写对象（先建仓库）、按 CAS 更新引用、写 `report-status`（`unpack ok` / `ok <ref>` / `ng <ref> <原因>`）。
- `http::App { store: Arc<Store>, config: Arc<Config> }`；`http::router(app: Arc<App>) -> axum::Router`；`http::serve(app, addr) -> Result<(), String>`（返回实际监听地址由测试用 `TcpListener` 绑定，端口 0）。
- 认证：`Authorization: Basic base64(user:pass)`；缺失或错误返回 401 与 `WWW-Authenticate: Basic realm="git"`。
- 路由：`GET /{repo}/info/refs?service=...`、`POST /{repo}/git-upload-pack`、`POST /{repo}/git-receive-pack`；请求体上限 `GIT_MAX_PUSH_BYTES`（默认 268435456）。

- [ ] **Step 1: 写失败测试**（`tests/e2e_http.rs`）：
  - `wrong_password_gets_401`：原始 HTTP 请求无凭据与错凭据均 401。
  - `first_push_creates_repo_and_ls_remote_lists_main`：临时工作仓库 `git init -b main`、提交、`git push http://git:git123@127.0.0.1:{port}/demo.git main` 成功；`git ls-remote` 输出包含 `refs/heads/main`。
  - `clone_returns_same_commit_and_bytes`：另一目录 `git clone`，`git rev-parse HEAD` 相等，文件字节一致；再提交一次并 `git push`，原克隆 `git pull` 得到新提交。
  - `push_with_stale_old_value_is_rejected`：先克隆两份，各自提交，先推成功、后推被拒绝（`git push` 非零退出），远端仍指向先到的提交。
- [ ] **Step 2: 运行确认失败**。
- [ ] **Step 3: 实现**：axum 路由与 `spawn_blocking` 调用同步核心；`info/refs` 按 service 返回对应 Content-Type 与 `# service=...` 前缀；POST 读取请求体后交给 `push::receive`。
- [ ] **Step 4: 运行确认通过**：`cargo test --test e2e_http`。

### Task 6: 拉取核心

**Files:**
- Create: `src/git/fetch.rs`
- Modify: `src/http.rs`（接入 upload-pack）
- Test: `tests/e2e_http.rs` 增加用例

**Interfaces:**
- `fetch::upload(store: &Store, repo: &str, input: &mut dyn BufRead, out: &mut dyn Write, max: usize) -> Result<(), String>`：读取 `want`/`have`/`done` 与能力；忽略 haves；计算 wants 可达对象；写 `NAK` pkt 后紧跟原始 pack 字节流。

- [ ] **Step 1: 写失败测试**：`clone_empty_repo_succeeds`（先推送建仓再删分支场景可跳过，改为直接对空通告仓库 `git clone` 断言空仓库提示）；`clone_annotated_tag_roundtrip`：创建注释标签与轻量标签并推送，克隆后 `git tag -l` 与 `git fsck --strict` 通过；`fetch_incremental_gets_only_new_commit`：`git fetch` 后新提交可见；`clone_preserves_binary_and_crlf_bytes`（含 NUL、CRLF、无结尾换行）。
- [ ] **Step 2: 运行确认失败**。
- [ ] **Step 3: 实现**：能力行只广告 `symref`、`agent`；解析 wants 时校验对象存在；pack 编码复用 Task 4。
- [ ] **Step 4: 运行确认通过**。

### Task 7: SSH 入口

**Files:**
- Create: `src/ssh.rs`, `tests/ssh_git.exp`, `tests/e2e_ssh.rs`
- Modify: `src/main.rs`（同时启动 HTTP 与 SSH）

**Interfaces:**
- `ssh::serve(store: Arc<Store>, config: Arc<Config>) -> Result<(), String>`：监听 `SSH_ADDR`；密码认证；仅接受 `git-upload-pack '<path>'` / `git-receive-pack '<path>'`。
- 主机密钥存 `settings["ssh_host_key"]`；不存在时生成 Ed25519 并保存。
- 连接内：`Handler` 把通道数据送入同步读取端，协议线程写出的字节经 `Handle::data` 发回，结束后发送 EOF 与关闭。

- [ ] **Step 1: 写失败测试**（`tests/e2e_ssh.rs`，用 `GIT_SSH_COMMAND` 指向 expect 包装脚本）：
  - `ssh_push_then_http_clone_sees_same_commit`：SSH 推送，HTTP 克隆核对提交。
  - `ssh_clone_sees_http_pushed_commit`：HTTP 推送，SSH 克隆核对。
  - `ssh_wrong_password_fails`：错误密码时 `git ls-remote` 非零退出。
  - `ssh_exec_rejects_other_commands`：用 `ssh` 执行 `rm -rf /` 之类命令被拒绝（返回非零）。
  - `ssh_host_key_survives_restart`：同一数据库重启后主机密钥文本不变。
- [ ] **Step 2: 运行确认失败**。
- [ ] **Step 3: 实现**：russh `Server`/`Handler`；命令解析只接受单个参数并去掉引号与可选前导 `/`；错误时 `channel_failure` 并关闭。
- [ ] **Step 4: 运行确认通过**。

### Task 8: 迁移验收与文档

**Files:**
- Create: `scripts/acceptance_pg.sh`（Docker 启动 PostgreSQL、运行测试、`pg_dump`/`psql` 恢复到第二库、再克隆比对）
- Create: `/CLAUDE.md`（L1）、`src/git/CLAUDE.md` 等 L2 补齐、`README.md` 完整用法
- Test: `tests/e2e_migration.rs`

**Interfaces:**
- 测试辅助：`start_server(db_url, http_addr_port0)` 返回实际地址与关闭句柄；SQLite 文件复制用于迁移。

- [ ] **Step 1: 写失败测试**：`sqlite_backup_restores_full_repository`：推送提交与标签后停止服务，复制数据库文件，用新文件启动新实例，克隆后 `rev-parse HEAD`、`git tag`、`git fsck --strict` 与原仓库一致，并能在新实例继续推送；`restart_keeps_refs`：同一数据库重启后克隆一致。
- [ ] **Step 2: 运行确认失败**。
- [ ] **Step 3: 实现**：测试辅助函数；`scripts/acceptance_pg.sh` 使用 `docker run --rm -d -p 127.0.0.1::5432 -e POSTGRES_PASSWORD=... postgres:17-alpine`，等待 `pg_isready`，运行 `TEST_PG_URL=... cargo test`，再用 `pg_dump` 导出并在第二个容器恢复，启动服务克隆比对，最后清理容器。
- [ ] **Step 4: 运行确认通过**：`cargo test`；`bash scripts/acceptance_pg.sh`。
- [ ] **Step 5: 文档收尾**：README 写本机 SQLite 启动、Docker PostgreSQL 启动、HTTPS/SSH 推拉示例、迁移步骤；L1/L2/L3 结构与代码一致。

## Self-Review

- 规格覆盖：双协议入口（Task 5、7）、首次推送建仓（Task 5）、数据库全量存储（Task 2、3、4）、SQLite 与 PostgreSQL（Task 1、2、8）、固定账号（Task 1、5、7）、迁移（Task 8）、标签与并发（Task 5、6）。
- 类型一致性：`Store`/`GitObject`/`GitKind`/`Service` 在各任务中名称一致；`pack::decode` 的 `lookup` 签名与 `Store::get_object` 对齐。
- 未覆盖项与测试：空的计划外输入已放入 Review Focus，并在对应任务列出用例。
