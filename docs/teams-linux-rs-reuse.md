# Reusing teams-linux-rs discoveries

The first increment supports Microsoft Graph and a separate native Teams chat
adapter. Microsoft rejected the built-in, first-party Teams application ID for
the MCP's requested Graph permissions with `AADSTS65002` during the Linux pilot.
The native Teams sign-in and one selected chat read succeeded in that pilot.
The desktop client also uses native Teams credentials to send messages. The
user approved using a similarly privileged credential while the first increment
exposed read-only operations. Inline image reads and assisted sends came later.
The credential itself is not restricted to reading.
Tenant policies and permission to use the integration still apply.

The implementation is informed by the existing project's discoveries:

- **Session isolation:** use a dedicated credential-store entry and avoid
  copying the desktop client's browser profile or saved session. Browser sign-in
  can launch its existing authentication-window helper; the approved application
  ID is built in while the MCP session remains separate. For Graph, an authorized
  app registration must be configured instead, as described above.
- **History security:** constrain pagination to the expected service origin and
  exact resource before forwarding a token. The Graph adapter validates the
  cursor before obtaining a token; the native adapter first obtains its regional
  service session, then validates the cursor before forwarding its token.
- **Messages and threads:** keep channel roots and replies distinct, expose
  source identity and timestamps, bound content, and indicate incomplete pages.
- **Events:** retain the original project's reconnection and reconciliation
  findings for the planned monitor; no real-time transport is exposed yet.
- **Transport limitations:** native mode supports text and inline-image chat
  reads, and assisted plain-text sends after confirmation. Image host checks
  are in [images.md](images.md). It does not implement channels or channel
  replies. A single live chat read does not establish completeness or
  behavior across tenants.

No source code was copied from `teams-linux-rs` or Better Teams for this
increment. The public application ID was embedded with the user's explicit
approval; no other environment values were imported. The shared findings,
provenance, license boundaries, source links,
and differences between the local and published source revisions are documented
in [PLAN.md](../PLAN.md). Any future code reuse requires a file-by-file license
and dependency review.
