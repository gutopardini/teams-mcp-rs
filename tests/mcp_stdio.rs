use std::{process::Stdio, time::Duration};

use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines},
    process::{Child, ChildStdin, ChildStdout, Command},
};

struct TestClient {
    child: Child,
    input: ChildStdin,
    output: Lines<BufReader<ChildStdout>>,
}

impl TestClient {
    async fn start() -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_teams-mcp"))
            .args(["serve", "--demo"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let input = child.stdin.take().unwrap();
        let output = BufReader::new(child.stdout.take().unwrap()).lines();
        Self {
            child,
            input,
            output,
        }
    }

    async fn send(&mut self, value: Value) {
        self.input
            .write_all(value.to_string().as_bytes())
            .await
            .unwrap();
        self.input.write_all(b"\n").await.unwrap();
        self.input.flush().await.unwrap();
    }

    async fn response(&mut self, id: u64) -> Value {
        loop {
            let line = tokio::time::timeout(Duration::from_secs(10), self.output.next_line())
                .await
                .expect("MCP server timed out")
                .unwrap()
                .expect("MCP server closed stdout");
            let value: Value =
                serde_json::from_str(&line).expect("stdout must contain only MCP JSON");
            if value.get("id").and_then(Value::as_u64) == Some(id) {
                return value;
            }
        }
    }

    async fn call(&mut self, id: u64, method: &str, params: Value) -> Value {
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))
            .await;
        self.response(id).await
    }
}

#[tokio::test]
async fn demo_server_lists_chat_tools_and_reads_without_an_allowlist() {
    let mut client = TestClient::start().await;
    let init = client
        .call(
            1,
            "initialize",
            json!({
                "protocolVersion": "2025-11-25", "capabilities": {},
                "clientInfo": {"name": "fictional-test-client", "version": "1.0"}
            }),
        )
        .await;
    assert!(init.get("result").is_some(), "{init}");
    client
        .send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
        .await;

    let listed = client.call(2, "tools/list", json!({})).await;
    let tools = listed["result"]["tools"].as_array().unwrap();
    let names: Vec<&str> = tools
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    assert_eq!(names.len(), 13);
    for name in [
        "teams_status",
        "teams_list_teams",
        "teams_list_channels",
        "teams_list_chats",
        "teams_get_messages",
        "teams_get_context",
        "teams_search_messages",
        "teams_get_thread",
        "teams_resolve_target",
        "teams_search_open_chats",
        "teams_find_messages",
        "teams_poll_watch",
        "teams_propose_reply",
    ] {
        assert!(names.contains(&name), "missing {name}");
    }
    assert!(
        !names.iter().any(|name| name.contains("send")
            || name.contains("approve")
            || name.contains("delete"))
    );
    assert!(tools.iter().all(|tool| {
        let read_only = tool["annotations"]["readOnlyHint"] == true;
        if tool["name"] == "teams_propose_reply" {
            !read_only
        } else {
            read_only
        }
    }));

    let status = client
        .call(
            3,
            "tools/call",
            json!({"name": "teams_status", "arguments": {}}),
        )
        .await;
    assert_eq!(status["result"]["structuredContent"]["backend"], "demo");
    let allowed = json!({"kind": "chat", "chat_id": "19:fictional-chat@thread.v2"});
    let read = client
        .call(
            4,
            "tools/call",
            json!({
                "name": "teams_get_messages", "arguments": {"target": allowed, "limit": 1}
            }),
        )
        .await;
    assert_eq!(
        read["result"]["structuredContent"]["data"]["messages"][0]["id"],
        "demo-message-1"
    );
    assert_eq!(
        read["result"]["structuredContent"]["data"]["messages"][0]["content_trust"],
        "untrusted"
    );
    assert_eq!(
        read["result"]["structuredContent"]["data"]["coverage"],
        "chat_messages"
    );
    assert_eq!(
        read["result"]["structuredContent"]["data"]["coverage"],
        "chat_messages"
    );

    let outside = client.call(5, "tools/call", json!({
        "name": "teams_get_messages", "arguments": {"target": {"kind": "chat", "chat_id": "19:other@thread.v2"}}
    })).await;
    assert!(outside["result"].get("isError").is_none() || outside["result"]["isError"] == false);
    assert_eq!(
        outside["result"]["structuredContent"]["data"]["messages"][0]["id"],
        "demo-message-1"
    );

    let thread = client.call(6, "tools/call", json!({
        "name": "teams_get_thread", "arguments": {
            "team_id": "00000000-0000-0000-0000-000000000003",
            "channel_id": "19:fictional-channel@thread.tacv2", "root_message_id": "demo-message-1"
        }
    })).await;
    assert_eq!(
        thread["result"]["structuredContent"]["data"]["replies"][0]["root_message_id"],
        "demo-message-1"
    );

    let channel = client
        .call(
            7,
            "tools/call",
            json!({
                "name": "teams_get_context", "arguments": {"target": {
                    "kind": "channel", "team_id": "00000000-0000-0000-0000-000000000003",
                    "channel_id": "19:fictional-channel@thread.tacv2"
                }}
            }),
        )
        .await;
    assert_eq!(
        channel["result"]["structuredContent"]["data"]["coverage"],
        "channel_root_posts_only"
    );

    let search = client
        .call(
            8,
            "tools/call",
            json!({
                "name": "teams_search_messages", "arguments": {
                    "target": {"kind": "chat", "chat_id": "19:fictional-chat@thread.v2"},
                    "query": "FICTIONAL", "max_pages": 1
                }
            }),
        )
        .await;
    assert_eq!(
        search["result"]["structuredContent"]["data"]["hits"][0]["id"],
        "demo-message-1"
    );
    assert_eq!(
        search["result"]["structuredContent"]["data"]["complete"],
        true
    );

    let resolved = client
        .call(
            9,
            "tools/call",
            json!({
                "name": "teams_resolve_target",
                "arguments": {"query": "Example chat"}
            }),
        )
        .await;
    assert_eq!(
        resolved["result"]["structuredContent"]["data"]["status"],
        "resolved"
    );
    let ambiguous = client
        .call(
            10,
            "tools/call",
            json!({
                "name": "teams_resolve_target",
                "arguments": {"query": "example"}
            }),
        )
        .await;
    assert_eq!(
        ambiguous["result"]["structuredContent"]["data"]["status"],
        "needs_confirmation"
    );
    assert!(ambiguous["result"]["structuredContent"]["data"]["selected"].is_null());
    let open = client
        .call(
            12,
            "tools/call",
            json!({
                "name": "teams_search_open_chats",
                "arguments": {"query": "josé"}
            }),
        )
        .await;
    assert_eq!(
        open["result"]["structuredContent"]["data"]["status"],
        "resolved"
    );
    assert_eq!(
        open["result"]["structuredContent"]["data"]["candidates"][0]["label"],
        "Allan and José"
    );
    assert_eq!(
        open["result"]["structuredContent"]["data"]["candidates"][0]["authorized"],
        false
    );
    let several = client
        .call(
            13,
            "tools/call",
            json!({
                "name": "teams_search_open_chats",
                "arguments": {"query": "a"}
            }),
        )
        .await;
    assert_eq!(
        several["result"]["structuredContent"]["data"]["status"],
        "needs_confirmation"
    );
    assert!(several["result"]["structuredContent"]["data"]["selected"].is_null());
    let proposal = client
        .call(
            11,
            "tools/call",
            json!({
                "name": "teams_propose_reply",
                "arguments": {
                    "target": allowed,
                    "text": "A fictional reply that must not be sent.",
                    "evidence": [{"message_id": "demo-message-1", "excerpt": "fictional incident"}]
                }
            }),
        )
        .await;
    assert_eq!(proposal["result"]["isError"], true);
    assert!(
        proposal["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("interaction_read_only")
    );

    client.child.kill().await.unwrap();
}
