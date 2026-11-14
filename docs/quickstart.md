# Linux terminal quickstart

This guide sets up the native Teams chat backend and a local Codex MCP client.
The examples use placeholder account IDs. Keep your real configuration outside
the repository; it contains account and conversation identifiers, not tokens.

## 1. Build and prepare the account

You need Rust 1.88 or later, Cargo, a Linux Secret Service keyring, and
`secret-tool`. From the repository root, build the executable:

```sh
cargo build --release --locked
```

Find the tenant UUID and your user object UUID for the intended work or school
account. If Azure CLI is installed and already signed in to the intended
account and tenant, these read-only commands show those values:

```sh
az account show --query tenantId --output tsv
az ad signed-in-user show --query id --output tsv
```

Verify that the selected Azure account and tenant are the ones you intend to
use. The object ID is a UUID, not an email address. You can obtain these values
through another authorized account or administrator workflow instead.

## 2. Create a private configuration

Choose a private location and create the directory:

```sh
MCP_CONFIG="$HOME/.config/teams-mcp-rs/config.toml"
mkdir -p "$(dirname "$MCP_CONFIG")"
```

Run the subsequent commands in the same shell session, or set `MCP_CONFIG`
again in a new shell.

Create the file with your editor and replace the placeholders below. If you
already have a configuration, use its path instead of overwriting it.

```toml
language = "en-US"
backend = "native_teams"

[identity]
tenant_id = "<tenant-uuid>"
account_id = "<user-object-uuid>"
```

Then restrict access to the file:

```sh
chmod 600 "$MCP_CONFIG"
```

The built-in Teams application ID is used when `client_id` is omitted. Native
mode requests the Teams `/.default` resource plus OpenID identity scopes. The
native credential can permit actions beyond reading, including sending; the MCP
currently exposes only reading tools. Use this integration only where authorized
by the account owner and the tenant's policies. The server's allowlist does not
reduce the privileges of the underlying Microsoft credential.

## 3. Sign in and choose a chat

Start device-code sign-in before selecting a chat:

```sh
target/release/teams-mcp login --config "$MCP_CONFIG" --bootstrap-chat
```

The command prints a Microsoft sign-in URL and a short code, and attempts to open
the default browser. Complete sign-in yourself in the browser; if it does not
open, visit the printed URL manually. The MCP checks the configured account and
stores its own refresh token in Secret Service. It does not import the desktop
client's saved session or browser profile.

List up to the first 100 recent native chat entries in the terminal without
changing the MCP allowlist:

```sh
target/release/teams-mcp choose-chat --config "$MCP_CONFIG" --list
```

The table shows the topic, last sender, last activity, and whether replies to
that chat are already allowed. A direct conversation may be labelled `Chat`; the last
sender is not necessarily the other participant. The directory response may
include message previews, but the listing does not print message bodies. This
first page is not a complete directory of all chats.

To select a chat, run the command without `--list` and enter its number from
the displayed table. Press Enter to cancel:

```sh
target/release/teams-mcp choose-chat --config "$MCP_CONFIG"
```

Selection adds the chat to the reply allowlist and performs a short read test;
the test reports the message count without printing message bodies. Reading
works before that selection. Existing reply entries stay in place. To remove
one, edit its `[[chats]]` entry in the private configuration and restart all
affected MCP server processes. For unnamed chats, labels are local and may be
edited to make them easier to distinguish.

## 4. Register with Codex and test

From the repository root, register the built binary and the configuration with
Codex. The following command creates a server entry named `teams-mcp`:

```sh
codex mcp add teams-mcp -- "$(pwd)/target/release/teams-mcp" serve --config "$MCP_CONFIG"
codex mcp get teams-mcp
```

If `teams-mcp` is already configured, inspect the existing entry before making
changes. Start a new Codex session to load the configured server. In the Codex
terminal UI, `/mcp` shows the active servers. You can then ask, for example:

> Use the Teams MCP to find the open chat I name and summarize its latest
> messages. Cite the source messages when available. Reading does not require
> the chat to be on the allowlist.

The MCP listing tools show only the local allowlist, not the entire directory.
The local MCP can pass selected message content to its AI client; whether that
client or its provider retains data depends on their own configuration and
policies. Native mode reads text and inline images from chats. Channels, native
thread replies, message sending, drafts, approval, and automation are not
implemented. Attachment counts and unmarked deletion status can be unknown.
Image blocks are capped at four per response.

The server reads its configuration once when it starts. After authorizing or
removing a chat, restart the relevant MCP server process or Codex session so
the new allowlist is loaded.

## 5. Sign out

Stop all running instances of this MCP server first, then remove its local
Secret Service entry:

```sh
target/release/teams-mcp logout --config "$MCP_CONFIG"
```

This clears the MCP's local refresh token; it does not revoke a Microsoft-side
grant or remove already cached tokens from running processes. For Microsoft-side
revocation, use the appropriate account or administrator controls.

For error codes, Graph configuration, security boundaries, and detailed reading
limits, see [operations.md](operations.md) and [implementation.md](implementation.md).
