#!/usr/bin/env bash
#
# PostgreSQL 迁移验收。
# 步骤：SQLite 源库推送 → 镜像迁移到 PostgreSQL → pg_dump 恢复第二个数据库
#      → 克隆比对 → 恢复后继续推送 → SSH 克隆。
# 依赖：docker、git、curl、OpenSSH（ssh、ssh-keyscan）、cargo。
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
CONTAINER=${GITDB_ACCEPTANCE_CONTAINER:-gitdb-acceptance-pg}
PG_PORT=${GITDB_ACCEPTANCE_PG_PORT:-55439}
SQLITE_HTTP_PORT=${GITDB_ACCEPTANCE_SQLITE_HTTP:-31801}
SQLITE_SSH_PORT=${GITDB_ACCEPTANCE_SQLITE_SSH:-22801}
PG_HTTP_A=${GITDB_ACCEPTANCE_PG_HTTP_A:-31802}
PG_SSH_A=${GITDB_ACCEPTANCE_PG_SSH_A:-22802}
PG_HTTP_B=${GITDB_ACCEPTANCE_PG_HTTP_B:-31803}
PG_SSH_B=${GITDB_ACCEPTANCE_PG_SSH_B:-22803}
PG_USER=gitdb
PG_PASSWORD=gitdb
DB_SOURCE=gitdb_acceptance_source
DB_RESTORED=gitdb_acceptance_restored
DB_TESTS=gitdb_acceptance_tests

WORK=$(mktemp -d "${TMPDIR:-/tmp}/gitdb-acceptance.XXXXXX")
PIDS=()
SUCCESS=0
LAST_PID=

cleanup() {
    for pid in "${PIDS[@]:-}"; do
        kill "$pid" 2>/dev/null || true
    done
    wait 2>/dev/null || true
    docker rm -f "$CONTAINER" >/dev/null 2>&1 || true
    if [ "$SUCCESS" -eq 1 ]; then
        rm -rf "$WORK"
    else
        echo "验收失败，工作目录保留: $WORK" >&2
    fi
}
trap cleanup EXIT

fail() {
    echo "验收失败: $*" >&2
    exit 1
}

wait_for_http() {
    local port=$1 pid=$2
    local _ attempt
    for attempt in $(seq 1 100); do
        if curl -fsS -u git:git123 -o /dev/null "http://127.0.0.1:$port/api/repos" 2>/dev/null; then
            return 0
        fi
        if ! kill -0 "$pid" 2>/dev/null; then
            echo "服务进程 $pid 提前退出" >&2
            return 1
        fi
        sleep 0.1
    done
    echo "等待服务超时: 端口 $port" >&2
    return 1
}

start_server() {
    local db_url=$1 http_port=$2 ssh_port=$3 log=$4
    DATABASE_URL="$db_url" \
        GIT_HTTP_ADDR="127.0.0.1:$http_port" \
        GIT_SSH_ADDR="127.0.0.1:$ssh_port" \
        "$ROOT/target/debug/gitdb" >"$WORK/$log" 2>&1 &
    LAST_PID=$!
    PIDS+=("$LAST_PID")
    wait_for_http "$http_port" "$LAST_PID"
}

stop_server() {
    local pid=$1
    kill "$pid" 2>/dev/null || true
    wait "$pid" 2>/dev/null || true
}

check_same_head() {
    local left=$1 right=$2
    local left_head right_head
    left_head=$(git -C "$left" rev-parse HEAD)
    right_head=$(git -C "$right" rev-parse HEAD)
    [ "$left_head" = "$right_head" ] ||
        fail "HEAD 不一致: $left_head != $right_head"
    echo "  HEAD 一致: $left_head"
}

check_repo() {
    local dir=$1
    git -C "$dir" fsck --strict >/dev/null
    [ "$(git -C "$dir" tag -l | sort | tr '\n' ' ')" = "light v1 " ] ||
        fail "标签不一致: $(git -C "$dir" tag -l)"
}

scan_host_key() {
    local port=$1 key=
    local _ attempt
    for attempt in $(seq 1 50); do
        key=$(ssh-keyscan -T 1 -p "$port" -t ed25519 127.0.0.1 2>/dev/null |
            awk '$2 == "ssh-ed25519" { print $3; exit }')
        [ -n "$key" ] && {
            echo "$key"
            return 0
        }
        sleep 0.1
    done
    return 1
}

command -v docker >/dev/null || fail "未找到 docker"
command -v git >/dev/null || fail "未找到 git"
command -v curl >/dev/null || fail "未找到 curl"
command -v ssh-keyscan >/dev/null || fail "未找到 ssh-keyscan"

echo "==> 准备隔离的 PostgreSQL 容器"
if docker container inspect "$CONTAINER" >/dev/null 2>&1; then
    label=$(docker inspect -f '{{ if index .Config.Labels "gitdb.acceptance" }}true{{ end }}' "$CONTAINER" 2>/dev/null || true)
    [ "$label" = "true" ] ||
        fail "容器 $CONTAINER 已存在且不是本脚本创建，请更换 GITDB_ACCEPTANCE_CONTAINER"
    docker rm -f "$CONTAINER" >/dev/null
fi
docker run -d --name "$CONTAINER" \
    --label gitdb.acceptance=true \
    -e "POSTGRES_USER=$PG_USER" \
    -e "POSTGRES_PASSWORD=$PG_PASSWORD" \
    -p "127.0.0.1:$PG_PORT:5432" \
    postgres:17-alpine >/dev/null
database_ready() {
    docker exec "$CONTAINER" psql -U "$PG_USER" -d postgres -tAc 'SELECT 1' >/dev/null 2>&1
}
ready=0
for attempt in $(seq 1 100); do
    if database_ready; then
        # 初始化阶段会先起一个临时实例再重启，等过一次重启窗口再做最终确认
        sleep 1
        if database_ready; then
            ready=1
            break
        fi
    fi
    [ "$(docker inspect -f '{{.State.Running}}' "$CONTAINER")" = "true" ] ||
        fail "PostgreSQL 容器提前退出"
    sleep 0.2
done
[ "$ready" -eq 1 ] || fail "PostgreSQL 未就绪"

run_sql() {
    local database=$1
    docker exec -i "$CONTAINER" psql -v ON_ERROR_STOP=1 -U "$PG_USER" -d "$database"
}
run_sql postgres <<< "CREATE DATABASE $DB_SOURCE;"
run_sql postgres <<< "CREATE DATABASE $DB_RESTORED;"
run_sql postgres <<< "CREATE DATABASE $DB_TESTS;"

echo "==> 构建 gitdb"
cargo build --quiet --manifest-path "$ROOT/Cargo.toml"
[ -x "$ROOT/target/debug/gitdb" ] || fail "未生成 target/debug/gitdb"

printf '#!/bin/sh\necho git123\n' >"$WORK/askpass.sh"
chmod 755 "$WORK/askpass.sh"
export GIT_SSH_COMMAND="ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -o PubkeyAuthentication=no -o PreferredAuthentications=password -o KbdInteractiveAuthentication=no -o NumberOfPasswordPrompts=3"
export SSH_ASKPASS="$WORK/askpass.sh"
export SSH_ASKPASS_REQUIRE=force
export DISPLAY=:0
export GIT_AUTHOR_NAME=Acceptance
export GIT_AUTHOR_EMAIL=acceptance@example.com
export GIT_COMMITTER_NAME=Acceptance
export GIT_COMMITTER_EMAIL=acceptance@example.com

echo "==> 启动 SQLite 源服务并推送"
start_server "$WORK/origin.sqlite3" "$SQLITE_HTTP_PORT" "$SQLITE_SSH_PORT" "server-sqlite.log"
SQLITE_PID=$LAST_PID
mkdir -p "$WORK/repo"
git -C "$WORK/repo" init -q -b main
printf 'before migration\n' >"$WORK/repo/readme.txt"
git -C "$WORK/repo" add readme.txt
git -C "$WORK/repo" commit -q -m "before migration"
git -C "$WORK/repo" tag -a v1 -m "release one"
git -C "$WORK/repo" tag light
git -C "$WORK/repo" push -q "http://git:git123@127.0.0.1:$SQLITE_HTTP_PORT/demo.git" main --tags
git clone -q "http://git:git123@127.0.0.1:$SQLITE_HTTP_PORT/demo.git" "$WORK/clone-sqlite"
check_repo "$WORK/clone-sqlite"

echo "==> SQLite → PostgreSQL 镜像迁移"
start_server "postgres://$PG_USER:$PG_PASSWORD@127.0.0.1:$PG_PORT/$DB_SOURCE" \
    "$PG_HTTP_A" "$PG_SSH_A" "server-pg-a.log"
PG_A_PID=$LAST_PID
git clone -q --mirror "http://git:git123@127.0.0.1:$SQLITE_HTTP_PORT/demo.git" "$WORK/mirror.git"
git -C "$WORK/mirror.git" push -q --mirror "http://git:git123@127.0.0.1:$PG_HTTP_A/demo.git"
git clone -q "http://git:git123@127.0.0.1:$PG_HTTP_A/demo.git" "$WORK/clone-pg"
check_same_head "$WORK/clone-sqlite" "$WORK/clone-pg"
check_repo "$WORK/clone-pg"
HOST_KEY_A=$(scan_host_key "$PG_SSH_A") || fail "无法读取 PostgreSQL 实例的主机密钥"
stop_server "$SQLITE_PID"
stop_server "$PG_A_PID"

echo "==> 运行 PostgreSQL 后端测试"
TEST_PG_URL="postgres://$PG_USER:$PG_PASSWORD@127.0.0.1:$PG_PORT/$DB_TESTS" \
    cargo test --quiet --manifest-path "$ROOT/Cargo.toml"

echo "==> pg_dump 导出并恢复到第二个数据库"
docker exec "$CONTAINER" pg_dump -U "$PG_USER" --no-owner --no-privileges "$DB_SOURCE" \
    >"$WORK/source.sql"
[ -s "$WORK/source.sql" ] || fail "pg_dump 输出为空"
run_sql "$DB_RESTORED" <"$WORK/source.sql"

echo "==> 启动恢复后的服务并验证"
start_server "postgres://$PG_USER:$PG_PASSWORD@127.0.0.1:$PG_PORT/$DB_RESTORED" \
    "$PG_HTTP_B" "$PG_SSH_B" "server-pg-b.log"
PG_B_PID=$LAST_PID
HOST_KEY_B=$(scan_host_key "$PG_SSH_B") || fail "无法读取恢复实例的主机密钥"
[ "$HOST_KEY_A" = "$HOST_KEY_B" ] || fail "恢复后 SSH 主机密钥发生变化"

git clone -q "http://git:git123@127.0.0.1:$PG_HTTP_B/demo.git" "$WORK/clone-restored"
check_same_head "$WORK/clone-pg" "$WORK/clone-restored"
check_repo "$WORK/clone-restored"

printf 'after restore\n' >"$WORK/clone-restored/readme.txt"
git -C "$WORK/clone-restored" commit -q -am "after restore"
git -C "$WORK/clone-restored" push -q origin main
NEW_HEAD=$(git -C "$WORK/clone-restored" rev-parse HEAD)

git clone -q "ssh://git@127.0.0.1:$PG_SSH_B/demo.git" "$WORK/clone-ssh"
[ "$(git -C "$WORK/clone-ssh" rev-parse HEAD)" = "$NEW_HEAD" ] ||
    fail "SSH 克隆未看到恢复后推送的提交"
git -C "$WORK/clone-ssh" fsck --strict >/dev/null
stop_server "$PG_B_PID"

SUCCESS=1
echo "==> PostgreSQL 迁移验收通过"
