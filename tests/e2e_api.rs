/**
 * [INPUT]: 依赖 common 测试装置、原始 HTTP 请求辅助与真实 git 客户端；依赖 serde_json 解析响应
 * [OUTPUT]: 验证 /api/repos 的认证、创建、列表、详情、改名、删除与错误状态
 * [POS]: 管理面 JSON API 的端到端验收；每个请求都是真实 TCP，不做处理函数级测试
 * [PROTOCOL]: 变更时更新此头部，然后检查 CLAUDE.md
 */
mod common;

use common::{
    basic_auth, git, git_ok, http_url, init_repo, raw_request, start_server, RawResponse,
    TestServer,
};
use serde_json::Value;

fn api(server: &TestServer, method: &str, path: &str, body: Option<&str>) -> RawResponse {
    raw_request(server.addr, method, path, Some(&basic_auth("git123")), body)
}

fn json(response: &RawResponse) -> Value {
    serde_json::from_str(&response.body).unwrap_or_else(|error| {
        panic!(
            "响应不是 JSON（状态 {}）: {error}\n{}",
            response.status, response.body
        )
    })
}

fn push_main(server: &TestServer, dir: &std::path::Path, repo: &str) -> String {
    let work = dir.join("work");
    let head = init_repo(&work, "readme.txt", b"hello\n");
    let output = git(&work, &["push", http_url(server, repo).as_str(), "main"]);
    assert!(
        output.status.success(),
        "推送失败: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    head
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn api_requires_the_same_basic_auth() {
    let server = start_server().await;
    assert_eq!(
        raw_request(server.addr, "GET", "/api/repos", None, None).status,
        401
    );
    assert_eq!(
        api(&server, "GET", "/api/repos", None).status,
        200,
        "正确账号应通过"
    );
    let wrong = basic_auth("wrong");
    assert_eq!(
        raw_request(server.addr, "GET", "/api/repos", Some(&wrong), None).status,
        401
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn create_list_and_read_repository() {
    let server = start_server().await;
    let created = api(&server, "POST", "/api/repos", Some(r#"{"name":"demo"}"#));
    assert_eq!(created.status, 201, "{}", created.body);
    let body = json(&created);
    assert_eq!(body["name"], "demo");
    assert!(body["created_at"].is_string());
    assert!(body["refs"].as_array().unwrap().is_empty());

    let conflict = api(
        &server,
        "POST",
        "/api/repos",
        Some(r#"{"name":"demo.git"}"#),
    );
    assert_eq!(conflict.status, 409, "同名仓库应冲突");

    let list = json(&api(&server, "GET", "/api/repos", None));
    let entries = list.as_array().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["name"], "demo");

    let detail = api(&server, "GET", "/api/repos/demo.git", None);
    assert_eq!(detail.status, 200, ".git 后缀应被接受");
    assert_eq!(json(&detail)["name"], "demo");

    assert_eq!(api(&server, "GET", "/api/repos/missing", None).status, 404);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn invalid_requests_are_rejected() {
    let server = start_server().await;
    for body in [
        r#"{"name":"bad name"}"#,
        r#"{"name":"../escape"}"#,
        r#"{"name":""}"#,
        r#"{}"#,
        "not json",
    ] {
        let response = api(&server, "POST", "/api/repos", Some(body));
        assert_eq!(response.status, 400, "{body} 应被拒绝");
        assert!(json(&response)["error"].is_string());
    }

    assert_eq!(
        api(&server, "PATCH", "/api/repos/demo", Some("{}")).status,
        400,
        "空修改应被拒绝"
    );
    assert_eq!(
        api(
            &server,
            "PATCH",
            "/api/repos/demo",
            Some(r#"{"default_branch":"dev"}"#)
        )
        .status,
        400,
        "默认分支字段已移除"
    );
    assert_eq!(
        api(
            &server,
            "DELETE",
            "/api/repos/demo",
            Some(r#"{"confirm":"demo"}"#)
        )
        .status,
        404
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rename_moves_repository_and_keeps_refs() {
    let server = start_server().await;
    let dir = tempfile::tempdir().unwrap();
    let head = push_main(&server, dir.path(), "demo");

    let renamed = api(
        &server,
        "PATCH",
        "/api/repos/demo",
        Some(r#"{"new_name":"team-notes"}"#),
    );
    assert_eq!(renamed.status, 200, "{}", renamed.body);
    let body = json(&renamed);
    assert_eq!(body["name"], "team-notes");
    let refs = body["refs"].as_array().unwrap();
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0]["name"], "refs/heads/main");
    assert_eq!(refs[0]["oid"], head);

    assert_eq!(
        api(&server, "GET", "/api/repos/demo", None).status,
        404,
        "旧名字应失效"
    );
    let remote = git_ok(
        dir.path(),
        &["ls-remote", http_url(&server, "team-notes").as_str()],
    );
    assert!(
        remote.contains(&format!("{head}\trefs/heads/main")),
        "{remote}"
    );

    assert_eq!(
        api(&server, "POST", "/api/repos", Some(r#"{"name":"other"}"#)).status,
        201
    );
    assert_eq!(
        api(
            &server,
            "PATCH",
            "/api/repos/team-notes",
            Some(r#"{"new_name":"other"}"#)
        )
        .status,
        409,
        "改名到已存在的仓库应冲突"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn delete_requires_confirmation_and_removes_repository() {
    let server = start_server().await;
    assert_eq!(
        api(&server, "POST", "/api/repos", Some(r#"{"name":"demo"}"#)).status,
        201
    );
    let dir = tempfile::tempdir().unwrap();
    push_main(&server, dir.path(), "demo");

    let refused = api(
        &server,
        "DELETE",
        "/api/repos/demo",
        Some(r#"{"confirm":"demo.git"}"#),
    );
    assert_eq!(refused.status, 400, "确认名必须精确匹配");
    assert!(server.store.repo_exists("demo").unwrap());

    let deleted = api(
        &server,
        "DELETE",
        "/api/repos/demo",
        Some(r#"{"confirm":"demo"}"#),
    );
    assert_eq!(deleted.status, 204);
    assert!(deleted.body.is_empty());
    assert!(server.store.get_repo("demo").unwrap().is_none());
    assert!(server.store.list_refs("demo").unwrap().is_empty());

    assert_eq!(api(&server, "GET", "/api/repos/demo", None).status, 404);
    let output = git(
        dir.path(),
        &["ls-remote", http_url(&server, "demo").as_str()],
    );
    assert!(!output.status.success(), "删除后拉取应失败");
}
