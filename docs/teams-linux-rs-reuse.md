# Reusing teams-linux-rs discoveries

The first read-only increment uses Microsoft Graph and an independent sign-in.
This makes the initial channel and chat reads testable against documented APIs
while preserving a separate boundary for a future adapter that reuses the
native Teams transport. Graph permissions and tenant consent remain prerequisites.

The implementation is informed by the existing project's discoveries:

- **Session isolation:** use a dedicated credential-store entry and avoid
  copying the desktop client's browser profile or saved session.
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
increment. The shared findings, provenance, license boundaries, source links,
and differences between the local and published source revisions are documented
in [PLAN.md](../PLAN.md). Any future code reuse requires a file-by-file license
and dependency review.
