#!/bin/sh
set -eu

fail() {
  printf '%s\n' 'managed_services_preflight_failed' >&2
  exit 1
}

owner_regular_0600() {
  value=$(stat -f '%u %Lp %l %HT' "$1" 2>/dev/null) || return 1
  [ "$value" = "$(id -u) 600 1 Regular File" ]
  [ ! -L "$1" ]
}

safe_executable() {
  file=$1
  value=$(stat -f '%u %Lp %l %HT' "$file" 2>/dev/null) || return 1
  set -- $value
  [ "$4 $5" = 'Regular File' ] || return 1
  [ "$3" = 1 ] || return 1
  [ "$1" = 0 ] || [ "$1" = "$(id -u)" ] || return 1
  [ ! -L "$file" ] || return 1
  mode=$2
  group=$(((mode / 10) % 10))
  other=$((mode % 10))
  [ $((group & 2)) = 0 ] && [ $((other & 2)) = 0 ] || return 1
  [ -x "$file" ]
}

config_dir="$HOME/Library/Application Support/${MEMEX_DATA_DIR_NAME:-LotsMemeX}/remote-managed"
config_file="$config_dir/remote-service.env"
[ -d "$config_dir" ] || fail
[ ! -L "$config_dir" ] || fail
[ "$(stat -f '%u %Lp %HT' "$config_dir" 2>/dev/null)" = "$(id -u) 700 Directory" ] || fail
owner_regular_0600 "$config_file" || fail
if grep -q 'MEMEX_RELAY_TRUST_PROXY' "$config_file"; then
  fail
fi

set -a
# shellcheck disable=SC1090
. "$config_file"
set +a

: "${MEMEX_REMOTE_REPOSITORY_ROOT:?}"
: "${MEMEX_REMOTE_EXPECTED_COMMIT:?}"
: "${MEMEX_REMOTE_NODE:?}"
: "${MEMEX_RELAY_ADMISSION_ROOT:?}"
: "${MEMEX_REMOTE_RELAY_PORT:?}"
: "${MEMEX_AGENT_SOCKET:?}"
: "${MEMEX_CLOUDFLARED_EXECUTABLE:?}"
: "${MEMEX_CLOUDFLARED_CONFIG:?}"
: "${MEMEX_CLOUDFLARED_CREDENTIALS:?}"
: "${MEMEX_REMOTE_BROWSER_ORIGIN:?}"
: "${MEMEX_REMOTE_WEB_CLIENT_URL:?}"
: "${MEMEX_REMOTE_RELAY_URL:?}"
: "${MEMEX_REMOTE_RELAY_HOSTNAME:?}"
case "$MEMEX_REMOTE_RELAY_HOSTNAME" in *[!a-z0-9.-]*) fail ;; esac
case "$MEMEX_REMOTE_BROWSER_ORIGIN" in https://*) ;; *) fail ;; esac
case "$MEMEX_REMOTE_WEB_CLIENT_URL" in "$MEMEX_REMOTE_BROWSER_ORIGIN"/*) ;; *) fail ;; esac
[ "$MEMEX_REMOTE_RELAY_URL" = "wss://$MEMEX_REMOTE_RELAY_HOSTNAME/relay" ] || fail

[ "$(printf '%s' "$MEMEX_RELAY_ADMISSION_ROOT" | wc -c | tr -d ' ')" = 43 ] || fail
case "$MEMEX_RELAY_ADMISSION_ROOT" in *[!A-Za-z0-9_-]*) fail ;; esac
case "$MEMEX_REMOTE_RELAY_PORT" in *[!0-9]*|'') fail ;; esac
[ "$MEMEX_REMOTE_RELAY_PORT" -ge 1024 ] && [ "$MEMEX_REMOTE_RELAY_PORT" -le 65535 ] || fail
[ -f "$MEMEX_REMOTE_REPOSITORY_ROOT/remote/relay/server.js" ] || fail
[ -f "$MEMEX_REMOTE_REPOSITORY_ROOT/remote/mac/bridge.js" ] || fail
[ "$(cd "$MEMEX_REMOTE_REPOSITORY_ROOT" && pwd -P)" = "$MEMEX_REMOTE_REPOSITORY_ROOT" ] || fail
[ ! -L "$MEMEX_REMOTE_REPOSITORY_ROOT" ] || fail
case "$MEMEX_REMOTE_EXPECTED_COMMIT" in
  *[!a-f0-9]*|'') fail ;;
esac
[ "$(printf '%s' "$MEMEX_REMOTE_EXPECTED_COMMIT" | wc -c | tr -d ' ')" = 40 ] || fail
[ "$(git -C "$MEMEX_REMOTE_REPOSITORY_ROOT" rev-parse HEAD 2>/dev/null)" = "$MEMEX_REMOTE_EXPECTED_COMMIT" ] || fail
[ -z "$(git -C "$MEMEX_REMOTE_REPOSITORY_ROOT" status --porcelain 2>/dev/null)" ] || fail
safe_executable "$MEMEX_REMOTE_NODE" || fail
safe_executable "$MEMEX_CLOUDFLARED_EXECUTABLE" || fail
owner_regular_0600 "$MEMEX_CLOUDFLARED_CONFIG" || fail
owner_regular_0600 "$MEMEX_CLOUDFLARED_CREDENTIALS" || fail

socket_state=$(stat -f '%u %Lp %HT' "$MEMEX_AGENT_SOCKET" 2>/dev/null) || fail
[ "$socket_state" = "$(id -u) 600 Socket" ] || fail
grep -Fqx "  - hostname: $MEMEX_REMOTE_RELAY_HOSTNAME" "$MEMEX_CLOUDFLARED_CONFIG" || fail
grep -Fq "service: http://127.0.0.1:$MEMEX_REMOTE_RELAY_PORT" "$MEMEX_CLOUDFLARED_CONFIG" || fail
grep -Fq 'service: http_status:404' "$MEMEX_CLOUDFLARED_CONFIG" || fail
grep -Fq "credentials-file: $MEMEX_CLOUDFLARED_CREDENTIALS" "$MEMEX_CLOUDFLARED_CONFIG" || fail
[ "$(wc -l < "$MEMEX_CLOUDFLARED_CONFIG" | tr -d ' ')" = 6 ] || fail

if [ "${MEMEX_PREFLIGHT_QUIET-0}" != 1 ]; then
  printf '%s\n' 'managed_services_preflight_ok'
fi
