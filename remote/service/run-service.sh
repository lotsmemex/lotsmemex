#!/bin/sh
set -eu

role=${1-}
case "$role" in
  relay|bridge|tunnel) ;;
  *) exit 64 ;;
esac

config_dir="$HOME/Library/Application Support/${MEMEX_DATA_DIR_NAME:-LotsMemeX}/remote-managed"
config_file="$config_dir/remote-service.env"
MEMEX_PREFLIGHT_QUIET=1 "$(dirname "$0")/preflight.sh"

# The file is owner-only, non-linked, and validated before it is trusted as shell input.
# It carries secrets only into child-process environment, never argv or logs.
set -a
# shellcheck disable=SC1090
. "$config_file"
set +a

case "$role" in
  relay)
    export HOST=127.0.0.1
    export PORT="$MEMEX_REMOTE_RELAY_PORT"
    export MEMEX_BROWSER_ORIGIN="$MEMEX_REMOTE_BROWSER_ORIGIN"
    unset MEMEX_RELAY_TRUST_PROXY
    unset MEMEX_DIAGNOSTIC_PUBLIC_URL MEMEX_REMOTE_DIAGNOSTIC_SAME_ORIGIN
    unset MEMEX_CLOUDFLARED_EXECUTABLE MEMEX_CLOUDFLARED_CONFIG MEMEX_CLOUDFLARED_CREDENTIALS
    exec "$MEMEX_REMOTE_NODE" "$MEMEX_REMOTE_REPOSITORY_ROOT/remote/relay/server.js"
    ;;
  bridge)
    export MEMEX_RELAY_URL="$MEMEX_REMOTE_RELAY_URL"
    export MEMEX_WEB_CLIENT_URL="$MEMEX_REMOTE_WEB_CLIENT_URL"
    unset MEMEX_CLOUDFLARED_EXECUTABLE MEMEX_CLOUDFLARED_CONFIG MEMEX_CLOUDFLARED_CREDENTIALS
    exec "$MEMEX_REMOTE_NODE" "$MEMEX_REMOTE_REPOSITORY_ROOT/remote/mac/bridge.js" run
    ;;
  tunnel)
    unset MEMEX_RELAY_ADMISSION_ROOT MEMEX_AGENT_SOCKET
    exec "$MEMEX_CLOUDFLARED_EXECUTABLE" --no-autoupdate \
      --config "$MEMEX_CLOUDFLARED_CONFIG" tunnel run
    ;;
esac
