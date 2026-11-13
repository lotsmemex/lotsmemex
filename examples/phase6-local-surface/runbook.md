# Synthetic local runbook

Run `cargo run -p memex-cli -- serve --library <synthetic-library> --max-sensitivity public`.
Read its one-line receipt, request `/`, `/api/v1/health`, and `/api/v1/catalog`
using its stated loopback URL, then terminate the foreground process. The
library is never written by these read routes.
