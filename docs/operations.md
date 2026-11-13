# Operations and troubleshooting

This guide describes the implemented read-only server. For setup prerequisites,
the demo, and an example MCP client configuration, see the [README](../README.md).
In the Linux pilot, native sign-in and one selected chat read succeeded against
one live tenant. Other accounts, tenants, and native channel reads remain
unvalidated.

## Configuration and access

- Supply a TOML file to `serve`, `login`, `choose-chat`, and `logout` with `--config`. The
  [example configuration](../config.example.toml) uses fictional values. The
  file contains IDs and labels, not tokens; keep your own file private.
- `tenant_id`, optional `client_id`, and `account_id` are UUIDs. The Teams Linux
  application ID is used when `client_id` is omitted; however, Microsoft rejected
  this first-party ID for the MCP's requested Graph permissions with `AADSTS65002`.
  For Graph, set `client_id` to a registration authorized for those delegated
  permissions. The account ID must be the user's object ID in the configured
  tenant. Graph verifies the account against `/me`; native mode checks the ID
  token returned directly by the Microsoft token endpoint over TLS, including
  tenant, account, audience, issuer, and validity before storing or using tokens.
- Each allowed channel is an exact team UUID and channel ID pair. Each allowed
  chat has an exact chat ID. `choose-chat --list` lists native chats in the
  terminal without changing the allowlist; `choose-chat` prompts in the terminal
  and adds one selected chat. MCP listing tools expose only allowlisted entries.
- Remove the example entries and add only the intended resources. At least one
  channel or chat is required for normal sign-in; `--bootstrap-chat` permits
  sign-in before choosing a chat. Native Teams credentials can have privileges
  beyond reading; the allowlist limits MCP operations rather than the grant.
  The configuration rejects unknown
  fields, duplicate resources, invalid IDs, and files larger than 128 KiB.
- `language` defaults to `en-US`. Only the English catalog is currently bundled;
  an unavailable valid locale falls back to English. CLI help and MCP tool
  descriptions remain English literals.

The server loads configuration **once at startup**. After changing the account,
language, or allowlist, restart all affected MCP server processes. In particular,
removing a resource from the file does not remove it from already running
processes. Adding the first chat or channel can require additional Graph consent;
sign in again if the refreshed token cannot obtain the necessary scope.

The allowlist limits operations exposed by this server. It does not reduce the
scope of a Microsoft grant, and it does not prevent another process with the same
OS-user privileges from changing configuration or accessing credentials. Use
process and credential isolation when that threat is relevant.

## Session lifecycle

`login` starts a separate browser or device-code flow only when explicitly run.
On Linux, `--webview-helper` can launch the existing Teams Linux authentication
window with PKCE. Device-code fallback opens the system browser. It stores
the refresh token in a dedicated Secret Service entry keyed by application,
tenant, client, and account. The server lazily obtains an access token on the
first authorized read, verifies the configured account, and caches the token in
memory until its calculated expiry. Status and configured-resource listing do
not check authentication or contact the service.

**To remove access from this machine:**

1. Stop all running instances of the MCP server, including those started by
   different MCP clients. A running instance can continue using a cached access
   token even after another process clears the stored refresh token.
2. Run `teams-mcp logout --config /absolute/path/to/config.toml` to clear this
   application's Secret Service entry for the configured identity.
3. If you need to revoke Microsoft-side grants or tokens, use the appropriate
   Microsoft account or administrator controls. Local logout does not revoke them.

The server does not intentionally persist conversation bodies to disk or print
them in logs. The MCP client, AI provider, operating system, or other software
may handle or retain data separately; their configuration is outside this server.

## Reading and search limits

- The maximum requested message or reply page is 50; the default is 25. The
  Graph response body limit is 2 MiB and each message body is limited to 8,000
  Unicode characters. `text_truncated` flags a shortened message.
- Graph channel pages contain root posts only, as identified by
  `coverage: channel_root_posts_only`. Use `teams_get_thread` for replies. Graph
  chat pages use `coverage: chat_messages`. Native chat pages use
  `coverage: native_chat_text_messages_only`; non-text events and unsupported
  media types are omitted and attachment counts and unmarked deletion status
  are unknown. Text with a native deletion marker is suppressed. Native channel
  reads and channel threads are not implemented.
- `next_cursor` is valid only for the same service origin and resource. Supply the
  same limit when following it. Invalid or modified destinations are rejected.
  `complete` on a message or thread page means that no next page was returned;
  check individual `text_truncated` flags and the coverage field separately.
- Search performs a Unicode lowercase substring comparison locally over fetched
  message text; it does not normalize accents or search attachments. It scans
  one to three pages of 25 messages from one allowed target and returns at most
  20 hits. For channels it searches root posts, not replies.
- Search reports `pages_scanned`, `messages_scanned`, `results_truncated`, and
  `source_text_truncated`. Its `complete` field is false if more pages exist,
  hits were omitted, or source message text was truncated. If hits were omitted
  within a page, narrow the query; the next-page cursor cannot recover them.

Message bodies, authors, links, and other remote fields are untrusted data. The
server does not open links or attachments and does not mark messages as read.

## Errors

Tool failures have `isError: true` and a JSON text payload containing a stable
`code`, an English `message`, and `upstream_status` when Graph returned a status.
The CLI prints a short English error to stderr; it does not print server bodies
or tokens for diagnosis.

| Code | Meaning and response |
| --- | --- |
| `forbidden` | The requested chat or channel is not in the loaded allowlist. Confirm the exact IDs and restart after authorized changes. |
| `invalid_input` | An identifier, page size, search query, or cursor length is invalid. Check the tool schema and bounds. |
| `unsupported_operation` | The selected backend does not implement this operation; native channel and thread reads are not yet available. |
| `invalid_cursor` | The cursor is malformed or targets a different origin, resource, or thread. Use an unmodified cursor returned for the same target. |
| `authentication_required` | No saved session was found. Run the explicit login command for this configuration. |
| `authentication_failed` | Sign-in, token refresh, or Graph account verification failed. Check tenant policy and consent, then sign in again if needed. |
| `incompatible_client` | Microsoft rejected the client ID for the requested permissions (`AADSTS65002`). Configure an authorized app registration and follow the tenant's consent process. |
| `account_mismatch` | The signed-in identity does not match the configured account, tenant, or other identity checks. Use the intended account and verify the configuration. |
| `credential_store_unavailable` | Secret Service or `secret-tool` could not be used. Check the Linux desktop keyring; tokens are not written to a plaintext fallback. |
| `unsupported_platform` | This platform's credential-store adapter is not implemented. The fictional demo remains available. |
| `upstream_error` | Graph returned an error. Inspect the numeric `upstream_status`: for example, `403` may indicate missing access or consent and `429` indicates throttling. The server does not automatically retry. |
| `network_error` | A Microsoft endpoint could not be reached or a response could not be read. Check connectivity and try again. |
| `response_too_large` | The response exceeded the size limit. Reduce the page size when possible. |
| `invalid_response` | The service returned data in an unsupported or inconsistent form. The server does not guess missing data. |
| `invalid_config` | The file is missing, invalid, or outside the configuration rules. Compare it with the example. |
| `no_resources` | Sign-in was requested with no allowed channels or chats. Configure at least one intended resource. |
| `protocol_error` | The MCP stdio transport failed. Check the client launch command and keep stdout reserved for MCP traffic. |

An access token can also be revoked before its cached expiration. If a running
server repeatedly receives an authentication error, restart it so it requests a
fresh token; sign in again if the stored session or consent is no longer valid.
