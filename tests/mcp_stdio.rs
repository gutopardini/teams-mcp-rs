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
async fn demo_server_exposes_only_read_tools_and_enforces_the_allowlist() {
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
    assert_eq!(names.len(), 8);
    for name in [
        "teams_status",
        "teams_list_teams",
        "teams_list_channels",
        "teams_list_chats",
        "teams_get_messages",
        "teams_get_context",
        "teams_search_messages",
        "teams_get_thread",
    ] {
        assert!(names.contains(&name), "missing {name}");
    }
    assert!(
        !names.iter().any(|name| name.contains("send")
            || name.contains("approve")
            || name.contains("delete"))
    );
    assert!(
        tools
            .iter()
            .all(|tool| tool["annotations"]["readOnlyHint"] == true)
    );

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

    let denied = client.call(5, "tools/call", json!({
        "name": "teams_get_messages", "arguments": {"target": {"kind": "chat", "chat_id": "19:other@thread.v2"}}
    })).await;
    assert_eq!(denied["result"]["isError"], true);
    assert!(
        denied["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("forbidden")
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

    client.child.kill().await.unwrap();
}
