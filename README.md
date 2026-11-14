# Teams MCP

A local [MCP](https://modelcontextprotocol.io) server that lets an AI chat use
Microsoft Teams without copying conversations by hand.

You ask things like “what did Ana say in the incidents chat?” or “reply in that
chat with this text.” The server resolves the chat, reads it with the signed-in
account, and posts a message only after you confirm the exact preview.

It is a Rust process on your machine. It speaks MCP over stdin and stdout. Your
refresh token stays in the operating-system keyring, not in the repository and
not in the chat transcript.

## What works today

Linux is the only platform with a real sign-in. The demo runs wherever Rust
builds, with fictional data and no network.

| You want | What the server does |
| --- | --- |
| Find a chat by its label | `teams_resolve_target` matches the allowlist. One exact match is selected. Anything else asks you to choose. |
| Find a chat the way the Teams sidebar does | `teams_search_open_chats` filters titles of chats already open in Teams. It does not search the company directory and does not read messages. |
| Read what one person said | `teams_find_messages` returns that person's messages only when the name matches one author. |
| Reply | `teams_propose_reply` shows the destination, the exact text, and quotes from real messages. The post happens only when the MCP client collects the word `send`. |
| Watch a code-review chat | `teams_poll_watch` records a baseline first and later returns only newer messages. It never sends. |
| See a picture posted in the chat | The read tools return up to four images as MCP image blocks, tied to the message id. The contract is in [docs/images.md](docs/images.md). |

Reads use the signed-in account and do not require an allowlist. The allowlist
limits replies and watches. Message text is marked untrusted: the model must
not follow instructions found inside Teams.

One Linux tenant has been used for a native sign-in, a selected chat read, and
one confirmed direct-message send. Graph chat access with the built-in Teams
application ID was rejected by Microsoft (`AADSTS65002`). Windows, macOS, other
tenants, channel reads on the native backend, and unattended sending are not
validated.

Details live in [the AI-chat guide](docs/ai-chat.md), [the image contract](docs/images.md),
[the plan](PLAN.md), and the [operations guide](docs/operations.md).

## Where this should go

The product we want is small enough that a person who never opens this
repository can install a binary, sign in, and let their AI client do the rest.

1. **The same flow on macOS and Windows.** The protocol and the tools are
   already shared. Sign-in still stops at the Linux keyring. The next
   credential stores are the macOS Keychain and Windows Credential Manager,
   each proven with a live sign-in before we call that platform supported.
2. **Finding a person stays a sidebar search.** Type part of a name, get the
   open chats whose titles match, and stop when more than one matches. No
   company-wide people search, and no scan of message history just to learn
   which chat is which.
3. **Confirmation stays in the AI client.** The preview and the word `send`
   should appear in the chat the person is already using. A side script that
   drives the server is a fallback, not the design.
4. **A reply can cite the conversation.** Evidence has to be a real excerpt.
   The server keeps refusing to post when the quote was not actually said.
5. **Code-review watches can wake an agent.** A configured chat can report
   that someone asked for a review. The watch never posts by itself. Any
   reply still goes through the same confirmation.
6. **Graph and native Teams stay explicit.** The config chooses one backend.
   Graph needs an application registration the tenant has authorized. Native
   Teams can read and, in assisted mode, post in a chat. Channel threads on
   the native backend are still future work.
7. **The allowlist limits replies and watches.** Discovering an open chat is
   enough to read it. Sending still requires the chat on the allowlist.

What we are not building: an unattended bot that posts while nobody is looking,
a copy of the Teams desktop client, or a place to store tokens in the repo.

## Requirements

- Rust 1.88 or later and Cargo.
- For a real account: Linux, Secret Service, and the `secret-tool` program.
  The demo does not need a keyring.
- A work or school account whose tenant allows the chosen application.
- The built-in application ID is the public Teams client ID, reused with
  explicit approval. Graph chat scopes need your own `identity.client_id`.
  The native backend does not.

The browser sign-in on Linux can reuse the `teams-linux-rs` window as a helper.
That helper has its own session. This server does not read the desktop
client's stored credentials.

## Try the demo

```sh
cargo run -- serve --demo
```

Point an MCP client at that process. It returns fictional chats and never
touches the network.

## Use your account

Copy `config.example.toml` somewhere outside this repository. Set `tenant_id`,
`account_id`, and `backend`. Use `native_teams` for the Teams chat service, or
`graph` with an authorized application registration. Delete the fictional
chats. Add a room only when the agent should reply there or watch it.

`interaction` defaults to `read_only`. Set it to `assisted` before a confirmed
reply can be posted. Restart the server after any config change.

```sh
cargo run -- login --config /absolute/path/to/config.toml --bootstrap-chat
cargo run -- choose-chat --config /absolute/path/to/config.toml --list
cargo run -- choose-chat --config /absolute/path/to/config.toml
cargo build --release
```

`choose-chat --list` prints recent chats and does not change the allowlist.
Without `--list`, you pick a number and that chat is added. A full terminal
walkthrough is in [docs/quickstart.md](docs/quickstart.md).

Register the release binary in the MCP client:

```json
{
  "mcpServers": {
    "teams": {
      "command": "/absolute/path/to/teams-mcp",
      "args": ["serve", "--config", "/absolute/path/to/config.toml"]
    }
  }
}
```

On Linux the process also needs `DBUS_SESSION_BUS_ADDRESS` and
`XDG_RUNTIME_DIR`, or Secret Service looks empty.

Sign out with:

```sh
cargo run -- logout --config /absolute/path/to/config.toml
```

That removes this application's keyring entry. It does not revoke the grant in
Microsoft's account portal. Stop every running server first, because a live
process can still hold an access token in memory.

## Tools

| Tool | Behavior |
| --- | --- |
| `teams_status` | Backend, account, resource counts, and whether replies need confirmation. It does not check that sign-in still works. |
| `teams_list_teams`, `teams_list_channels`, `teams_list_chats` | The allowlist only. They do not query Teams. |
| `teams_get_messages`, `teams_get_context` | One bounded page from a chat or, on Graph, a channel. The allowlist is not required. Inline images are image content blocks. |
| `teams_search_messages` | Local text search over at most three fetched pages of one chat or channel. Hits can include the same image blocks. |
| `teams_get_thread` | A channel post and a bounded page of replies. Images on the root and the replies share one four-image cap. |
| `teams_resolve_target` | One allowlisted label or exact ID. Otherwise, candidates and no selection. |
| `teams_search_open_chats` | Title filter of chats already open in Teams. Several matches, or an incomplete list, means no selection. |
| `teams_find_messages` | One author's messages. Several matching names return no message text. Pictures on the returned messages are included. |
| `teams_poll_watch` | A configured code-review watch. The first call only stores a baseline. Newer messages can include pictures. |
| `teams_propose_reply` | Preview a reply. Post it only after the client collects `send`. |

Pages hold at most 50 messages. Cursors are accepted only for the same service
and the same resource. A read attaches at most four images, each at most
1.5 MiB, from Teams media hosts or from Graph hosted content. Other links and
non-image files are not opened. Native results are text and inline images in
a chat. The server does not mark anything as read. The field-by-field contract
is [docs/images.md](docs/images.md).

## Security

The allowlist is checked before a token is used to send. A read of a
well-formed chat or channel id uses the signed-in account and does not consult
the allowlist. Pagination URLs must stay on the expected origin and path.
Redirects are not followed. An image URL is requested only after the host and
path checks in [docs/images.md](docs/images.md); the token for that request is
not sent to the address written in the message HTML.
Responses and message text are size-limited. The signed-in account must match
the config.

That is an application boundary, not a sandbox. A program running as the same
user can read the config and the keyring. Do not treat the server as isolation
from the rest of the desktop session.

The native Teams credential can do more than these tools expose. Assisted mode
is the only path that posts, and only after confirmation. There is no approval
flag an agent can set by itself.

## Development

```sh
cargo test
```

Tests use fictional data and local fakes. They do not sign in and they do not
send to Microsoft. Repository text is English.
