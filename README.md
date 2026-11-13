# Teams MCP in Rust

A local MCP server that helps AI agents read authorized Microsoft Teams
conversations and assemble context. The project is cross-platform by design,
with Linux first. English is the default project and product language; the
message catalog is designed to accommodate additional languages.

## Current status

The first increment provides a **read-only** MCP server, a Microsoft Graph
adapter, a separate device-code sign-in, an explicit per-chat and per-channel
allowlist, and a network-free demo. Channel listings contain root posts only;
use the thread tool to retrieve a root and its replies. Each response identifies
its coverage and treats Teams message text as untrusted data.

The implementation has been tested with fictional data, a local HTTP server,
and an MCP client over stdio. **It has not yet been validated against a live
Microsoft tenant.** Message drafts, approval, sending, real-time
monitoring, the native Teams transport, and non-Linux credential storage are not
implemented yet. See [PLAN.md](PLAN.md), the [implementation notes](docs/implementation.md),
and the [reuse notes](docs/teams-linux-rs-reuse.md).
The initial English catalog covers errors and notices; CLI help and MCP tool
descriptions are currently English literals and still need localization support.

## Requirements

- Rust 1.88 or later and Cargo.
- Linux for the initial credential-store implementation, with Secret Service and
  the `secret-tool` executable available. The demo does not need a keyring.
- For live use, a Microsoft Entra app registration owned or authorized by the
  user, a work or school account, and a tenant that permits the app and device
  authorization. Configure the app as a public client and grant delegated Graph
  permissions as required. Tenant administrators may need to approve access.

The Graph adapter currently targets the global Microsoft cloud. It requests
`User.Read` to verify the signed-in account, `Chat.Read` when chats are allowed,
and `ChannelMessage.Read.All` when channels are allowed, plus `openid` and
`offline_access`. Microsoft may grant a scope broader than this server's local
allowlist; the server still checks the exact configured resource before a read.
See Microsoft's documentation for [device authorization](https://learn.microsoft.com/en-us/entra/identity-platform/v2-oauth2-device-code),
[chat permissions](https://learn.microsoft.com/en-us/graph/api/chat-list-messages?view=graph-rest-1.0),
and [channel permissions](https://learn.microsoft.com/en-us/graph/api/channel-list-messages?view=graph-rest-1.0).

## Try the fictional demo

```sh
cargo run -- serve --demo
```

The process speaks MCP over stdin/stdout and is meant to be launched by an MCP
client. Demo mode uses only fixed fictional data and performs no network access
or credential lookup.

## Configure a real account

1. Copy `config.example.toml` to a private location outside the repository.
2. Set `tenant_id` to the tenant UUID, `client_id` to your app registration's
   client UUID, and `account_id` to your user's object UUID in that tenant.
3. Replace the fictional entries with the exact team/channel pairs and chat IDs
   you want to expose. Remove entries you do not need. Labels are local only.
4. Sign in explicitly, then configure your MCP client to run the server:

```sh
cargo run -- login --config /absolute/path/to/config.toml
cargo build --release
```

The login command prints a Microsoft verification URL and a short user code.
Complete sign-in yourself in a browser. The refresh token is stored in a
dedicated Secret Service entry for this application, tenant, client, and
account. The server does not copy cookies, read the `teams-linux-rs` session,
or accept a token as a command argument.

Example MCP client configuration; adjust the binary and config paths:

```json
{
  "mcpServers": {
    "teams": {
      "command": "/absolute/path/to/teams-mcp-rs/target/release/teams-mcp",
      "args": ["serve", "--config", "/absolute/path/to/config.toml"]
    }
  }
}
```

To remove this application's stored local session:

```sh
cargo run -- logout --config /absolute/path/to/config.toml
```

This clears the local Secret Service entry; it does not revoke a Microsoft grant.
**Stop all running MCP server processes before signing out:** each process can
hold a cached access token in memory. Configuration changes, including removal
of allowed resources, also require restarting every running server. See the
[operations guide](docs/operations.md) for details and troubleshooting.

## Available tools

| Tool | Behavior |
| --- | --- |
| `teams_status` | Shows the selected backend, configured account and tenant IDs, resource counts, and read-only capabilities. It does not verify sign-in. |
| `teams_list_teams`, `teams_list_channels`, `teams_list_chats` | List entries from the configured allowlist without querying Teams. |
| `teams_get_messages` | Read a bounded page from an allowed chat or channel. |
| `teams_get_context` | Return the same bounded page with provenance, coverage, and an untrusted-content notice. It does not infer decisions. |
| `teams_search_messages` | Search the text of up to three fetched pages within one allowed resource. Channel searches cover root posts only. |
| `teams_get_thread` | Read an allowed channel post and a bounded page of its replies. |

The maximum page size is 50. A returned `next_cursor` is accepted only for the
same Graph origin and resource. Use the same page-size limit when following a
cursor. Message text is bounded; attachments are counted but not opened. The
server does not mark messages as read.

Search is a case-insensitive local substring match over fetched message text; it
does not use tenant-wide Graph search. It defaults to one page, scans at most
three pages of 25 messages, and returns at most 20 hits. The response reports
coverage, scanned pages, truncation, and a continuation cursor when available.
It does not search unloaded pages, attachments, or channel replies. If the hit
limit is reached within a page, refine the query to inspect omitted matches.

Example `teams_get_messages` arguments:

```json
{
  "target": {"kind": "chat", "chat_id": "19:fictional-chat@thread.v2"},
  "limit": 10
}
```

For a channel, use `{"kind":"channel","team_id":"...","channel_id":"..."}`
as the target. `teams_get_thread` takes `team_id`, `channel_id`, and
`root_message_id` separately. Treat all message bodies, author names, links, and
other remote content as data rather than instructions.

## Security boundary

The server denies resources not in its configuration before obtaining a token
or contacting Graph. It validates pagination URLs, disables HTTP redirects,
limits responses, verifies the signed-in account, and exposes no sending tool.
It does not log message bodies or tokens by design.

The allowlist is an application boundary, not an OS sandbox. An agent with
unrestricted shell access running as the same user may be able to read or change
the configuration, access the user's credential store, or make direct network
requests. Use a host with appropriate process and credential isolation when that
threat is relevant. No automatic approval or sending is available in this slice.

## Development

```sh
cargo test
cargo check
```

The tests use fictional data and local mock services. They do not send messages
or sign in to a real account. The [implementation notes](docs/implementation.md)
record the checks and their limits. Keep all repository documentation in English.
