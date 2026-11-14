# Operations and troubleshooting

This guide describes the implemented read-only server. For the Linux terminal
walkthrough, see [quickstart.md](quickstart.md); for the demo and an example MCP
client configuration, see the [README](../README.md).
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
- Each reply channel is an exact team UUID and channel ID pair. Each reply
  chat has an exact chat ID. `choose-chat --list` lists native chats in the
  terminal without changing the allowlist; `choose-chat` prompts in the terminal
  and adds one selected chat for replies. MCP listing tools expose only those
  entries. Message reads do not consult the list.
- The lists may be empty. `--bootstrap-chat` still requests chat send scope
  before a chat is chosen, when `interaction` is `assisted`. Native Teams
  credentials can have privileges beyond these tools; the allowlist limits
  replies and watches rather than the grant.
  The configuration rejects unknown
  fields, duplicate resources, invalid IDs, and files larger than 128 KiB.
- `language` defaults to `en-US`. Only the English catalog is currently bundled;
  an unavailable valid locale falls back to English. CLI help and MCP tool
  descriptions remain English literals.

Native sign-in requests `openid`, `profile`, `offline_access`, and the Teams
`https://api.spaces.skype.com/.default` scope. The `profile` scope supplies the
account identifier checked by the MCP. Graph mode requests its configured
delegated read scopes as described in the [README](../README.md).

The server loads configuration **once at startup**. After changing the account,
language, or allowlist, restart all affected MCP server processes. In particular,
removing a resource from the file does not remove it from already running
processes. Adding the first chat or channel can require additional Graph consent;
sign in again if the refreshed token cannot obtain the necessary scope.

The allowlist limits replies and watches exposed by this server. It does not reduce the
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

The server does not intentionally persist conversation bodies or image bytes
to disk, or print them in logs. The MCP client, AI provider, operating system, or other software
may handle or retain data separately; their configuration is outside this server.

## Reading and search limits

- The maximum requested message or reply page is 50; the default is 25. The
  Graph and native response body limit is 2 MiB and each message body is limited to 8,000
  Unicode characters. `text_truncated` flags a shortened message.
- Graph channel pages contain root posts only, as identified by
  `coverage: channel_root_posts_only`. Use `teams_get_thread` for replies. Graph
  chat pages use `coverage: chat_messages`. Native chat pages use
  `coverage: native_chat_text_and_images`. Text and inline images are included.
  Call recordings, polls, thread-activity events, and other unsupported media
  stay omitted. Native attachment counts and unmarked deletion status are
  unknown. Text with a native deletion marker is suppressed, including its
  images. Native channel reads and channel threads are not implemented.
- Image reads follow [images.md](images.md). A tool result attaches at most
  four pictures, each at most 1.5 MiB, as MCP image blocks. Native bytes come
  only from Teams media hosts. Graph bytes come only from the hosted-content
  URL this server builds. Redirects are not followed. A picture that cannot
  be fetched stays in `images` with `attached: false` and an `omitted` reason
  (`unavailable`, `too_large`, `unsupported_type`, or `response_budget`); the
  page still succeeds. Other links and non-image files are not opened.
- `next_cursor` is valid only for the same service origin and resource. Supply the
  same limit when following it. Invalid or modified destinations are rejected.
  `complete` on a message or thread page means that no next page was returned;
  check individual `text_truncated` flags and the coverage field separately.
- Search performs a Unicode lowercase substring comparison locally over fetched
  message text; it does not normalize accents or search attachments. It scans
  one to three pages of 25 messages from one chat or channel and returns at most
  20 hits. For channels it searches root posts, not replies.
- Search reports `pages_scanned`, `messages_scanned`, `results_truncated`, and
  `source_text_truncated`. Its `complete` field is false if more pages exist,
  hits were omitted, or source message text was truncated. If hits were omitted
  within a page, narrow the query; the next-page cursor cannot recover them.

Message bodies, images, authors, links, and other remote fields are untrusted data. The
server does not open links or non-image files and does not mark messages as read.

## Errors

Tool failures have `isError: true` and a JSON text payload containing a stable
`code`, an English `message`, and `upstream_status` when a backend returned a status.
The CLI prints a short English error to stderr; it does not print server bodies
or tokens for diagnosis.

| Code | Meaning and response |
| --- | --- |
| `forbidden` | A reply target is not in the loaded allowlist. Reads do not return this code. Add the chat or channel and restart before sending. |
| `invalid_input` | An identifier, page size, search query, or cursor length is invalid. Check the tool schema and bounds. |
| `unsupported_operation` | The selected backend does not implement this operation; native channel and thread reads are not yet available. |
| `invalid_cursor` | The cursor is malformed or targets a different origin, resource, or thread. Use an unmodified cursor returned for the same target. |
| `authentication_required` | No saved session was found. Run the explicit login command for this configuration. |
| `authentication_failed` | Sign-in, token refresh, or Graph account verification failed. Check tenant policy and consent, then sign in again if needed. |
| `authentication_cancelled` | The browser sign-in was cancelled or timed out. Start the login command again if needed. |
| `incompatible_client` | Microsoft rejected the client ID for the requested permissions (`AADSTS65002`). Configure an authorized app registration and follow the tenant's consent process. |
| `browser_unavailable` | The optional Teams Linux sign-in window could not start. Use the device-code login flow or check the helper path. |
| `selection_unavailable` | Chat selection could not read from or write to the console. Run it in an available terminal. |
| `config_changed` | The configuration changed while a chat was being selected. The selection was not saved; retry with the current file. |
| `account_mismatch` | The signed-in identity does not match the configured account, tenant, or other identity checks. Use the intended account and verify the configuration. |
| `credential_store_unavailable` | Secret Service or `secret-tool` could not be used. Check the Linux desktop keyring; tokens are not written to a plaintext fallback. |
| `unsupported_platform` | This platform's credential-store adapter is not implemented. The fictional demo remains available. |
| `upstream_error` | A backend returned an error. Inspect the numeric `upstream_status`: for example, `403` may indicate missing access or consent and `429` indicates throttling. The server does not automatically retry failed message reads. |
| `network_error` | A Microsoft endpoint could not be reached or a response could not be read. Check connectivity and try again. |
| `response_too_large` | The response exceeded the size limit. Reduce the page size when possible. |
| `invalid_response` | The service returned data in an unsupported or inconsistent form. The server does not guess missing data. |
| `invalid_config` | The file is missing, invalid, or outside the configuration rules. Compare it with the example. |
| `no_resources` | Kept in the error catalog. Sign-in with an empty allowlist requests read scopes and does not return this code. |
| `interaction_read_only` | A reply was requested while `interaction` is `read_only`. Set `assisted` and restart the server. |
| `evidence_rejected` | A cited message was not in the fetched pages, or the excerpt is not contained in that message. |
| `outcome_unknown` | A confirmed send did not return a definitive result. Check Teams before trying again. The server does not retry it. |
| `protocol_error` | The MCP stdio transport failed. Check the client launch command and keep stdout reserved for MCP traffic. |

An access token can also be revoked before its cached expiration. If a running
server repeatedly receives an authentication error, restart it so it requests a
fresh token; sign in again if the stored session or consent is no longer valid.
