# Vendored dependencies

`shairplay/` is based on `shairplay` 0.10.0 (LGPL-3.0-or-later) and is kept
locally because the Windows receiver needs connection-lifecycle fixes that are
not present in the published crate.

Local changes are intentionally limited to AirPlay screen-mirroring stability:

- video reads no longer treat 30 seconds without media payload as a dead TCP connection;
- the active video task belongs to its RTSP connection and is cancelled on teardown/drop;
- AP2 `TEARDOWN` honors stream types so stopping video does not tear down unrelated audio/control state.

The upstream license is preserved in `shairplay/LICENSE`.
