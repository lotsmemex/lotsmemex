# memex.local_surface

`memex serve --library <root> --max-sensitivity <ceiling> [--port <0..65535>]
[--orb-portal]` starts a foreground server only on `127.0.0.1`. By default it
accepts only exact loopback Host/Origin values. `--orb-portal` is an explicit
Amp-orb demo mode that additionally accepts only the exact HTTPS `*.onamp.dev`
authority/origin injected by Amp; Amp provides the external Portal authentication.

`--lan-address <private-ipv4> --lan-hostname <bonjour.local>` is a direct owner-LAN
mode. It requires a stable high port, binds only that RFC1918 non-loopback address,
accepts only the exact Bonjour Host/Origin, and enables exact `POST /memex/capture`
through the standard owner-only daemon socket. It never opens queue SQLite. `/capture`
and `/memex` are aliases for the live Mobile Library; a successful Store commit is
visible on the next request without process restart or static export.

The surface exposes no generic raw/attachment/path, wildcard/public bind, production
relay, or deployment operation. Its sole attachment-byte
route is the Mobile Library contract's digest-addressed, current-revision,
Evidence-backed captured image read. Stopping the process closes the selected port.
