#!/bin/sh
set -eu

action=${1-}
service=${2-}
case "$service" in
  relay) label=com.lotsmemex.remote-relay ;;
  bridge) label=com.lotsmemex.remote-bridge ;;
  tunnel) label=com.lotsmemex.remote-tunnel ;;
  *) printf '%s\n' 'unknown_remote_service' >&2; exit 64 ;;
esac

domain="gui/$(id -u)"
plist="$HOME/Library/LaunchAgents/$label.plist"

safe_plist() {
  value=$(stat -f '%u %Lp %l %HT' "$plist" 2>/dev/null) || return 1
  [ "$value" = "$(id -u) 600 1 Regular File" ] || return 1
  [ ! -L "$plist" ] || return 1
  plist_label=$(/usr/libexec/PlistBuddy -c 'Print :Label' "$plist" 2>/dev/null) || return 1
  [ "$plist_label" = "$label" ]
}

case "$action" in
  status)
    launchctl print "$domain/$label" 2>/dev/null \
      | awk -v label="$label" '/^[[:space:]]*(state|pid) = / { print label, $1, $3 }'
    ;;
  start)
    safe_plist || exit 66
    launchctl bootstrap "$domain" "$plist"
    ;;
  stop)
    safe_plist || exit 66
    launchctl bootout "$domain" "$plist"
    ;;
  *) printf '%s\n' 'usage: service-control.sh status|start|stop relay|bridge|tunnel' >&2; exit 64 ;;
esac
