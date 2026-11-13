# Implementation and validation notes

This document describes the first read-only increment. [PLAN.md](../PLAN.md)
contains the broader design and future phases; [operations.md](operations.md)
describes configuration, session handling, and errors.

## Components

| File | Responsibility |
| --- | --- |
| `src/main.rs` | CLI subcommands: `serve`, explicit `login`, and `logout`. Server mode uses MCP over stdio. |
| `src/config.rs` | Parses and validates the explicit account and resource allowlist; derives requested delegated scopes. |
| `src/auth.rs` | Microsoft device-code and refresh flows, account verification, in-memory access-token cache, and the Linux Secret Service adapter. |
| `src/graph.rs` | Read-only Microsoft Graph requests, validated pagination, bounded responses, and message normalization. |
| `src/backend.rs` | Shared message models, access-policy checks before backend calls, thread reads, and bounded local search. |
| `src/demo.rs` | Fixed fictional data with no network or credential access. |
| `src/server.rs` | Read-only MCP tools and structured results or sanitized tool errors. |
| `src/locale.rs`, `locales/en-US.ftl` | English error and notice catalog with fallback infrastructure. Full CLI and tool-description localization is pending. |
| `tests/mcp_stdio.rs` | End-to-end MCP initialization, tool discovery, and fictional calls over stdio. |

The shared models and interfaces do not depend on a Linux GUI. The actual
credential store uses `secret-tool` on Linux; adapters for Windows and macOS are
not implemented or validated.

## Data flow and security

1. The server loads and validates configuration. Its MCP list tools expose only
   entries in that loaded allowlist; they do not discover resources remotely.
2. A message, context, thread, or search request is checked against the exact
   configured chat or channel before the backend can obtain an access token.
3. If needed, the authentication provider loads this application's refresh token,
   requests a Graph access token, and verifies the configured account through
   `GET /v1.0/me`. Refreshes are serialized within one server process.
4. Graph requests use HTTPS in production, do not follow redirects, and are
   bounded by request timeouts and response-size limits. A pagination cursor is
   checked against the expected origin and path before token acquisition.
5. Graph content is normalized with original identity, timestamps, available
   source links, truncation indicators, and untrusted-content markers. HTML is
   converted to text; links and attachments are not opened. Search compares
   locally fetched message text and does not issue a tenant-wide search query.

The Graph adapter reads `/chats/{id}/messages` or
`/teams/{team}/channels/{channel}/messages`, with separate root and reply
requests for channel threads. Authentication uses POST requests to Microsoft's
identity endpoints; there are no message-sending or mutation tools. The server
does not mark messages as read.

These checks are application guardrails, not protection against an unrestricted
agent running with the same OS-user privileges. Approval, a protected sending
gateway, automation, and a stronger isolation boundary are future work.

## Validation record

On November 13, 2026, commit `43d29d0` was checked on Linux with:

- Rust formatting checks and Clippy over all targets, with warnings denied.
- `cargo test --locked --offline`: 14 unit tests and one MCP stdio integration
  test passed. The tests cover allowlist denial, account mismatch, token refresh
  serialization, fictional device login and logout, safe pagination, message
  normalization, bounded search, locale fallback, and the read-only tool list.
- `cargo build --release --locked --offline`: the release build completed.

The authentication and Graph services in those tests were local fakes; the MCP
demo used fictional data. There was no sign-in to Microsoft, real message read,
message send, or validation of the actual Secret Service integration. Windows,
macOS, tenant-specific policies, and production performance remain unvalidated.
