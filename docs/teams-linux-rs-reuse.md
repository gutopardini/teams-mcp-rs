# Reusing teams-linux-rs discoveries

The first read-only increment uses Microsoft Graph and a separate sign-in.
Graph permissions and tenant consent remain prerequisites. The existing desktop
client uses native Teams services for chat history; this MCP does not yet have
that adapter. Microsoft rejected the built-in, first-party Teams application ID
for the MCP's requested Graph permissions with `AADSTS65002` during the Linux pilot.
The desktop client also uses its native Teams session to send messages, so an
MCP that reused this session would not establish that the underlying credential
is restricted to reading. Any native integration needs a separate assessment of
credential privileges, tenant authorization, and the read-only boundary.

The implementation is informed by the existing project's discoveries:

- **Session isolation:** use a dedicated credential-store entry and avoid
  copying the desktop client's browser profile or saved session. Browser sign-in
  can launch its existing authentication-window helper; the approved application
  ID is built in while the MCP session remains separate. For Graph, an authorized
  app registration must be configured instead, as described above.
- **History security:** constrain pagination to the expected service origin and
  exact resource, including validation before obtaining or forwarding a token.
- **Messages and threads:** keep channel roots and replies distinct, expose
  source identity and timestamps, bound content, and indicate incomplete pages.
- **Events:** retain the original project's reconnection and reconciliation
  findings for the planned monitor; no real-time transport is exposed yet.
- **Transport limitations:** the existing native channel-reply adapter has local
  fixture coverage but its live endpoint is not confirmed. This increment
  implements no sending and makes no claim that the native transport is validated.

No source code was copied from `teams-linux-rs` or Better Teams for this
increment. The public application ID was embedded with the user's explicit
approval; no other environment values were imported. The shared findings,
provenance, license boundaries, source links,
and differences between the local and published source revisions are documented
in [PLAN.md](../PLAN.md). Any future code reuse requires a file-by-file license
and dependency review.
