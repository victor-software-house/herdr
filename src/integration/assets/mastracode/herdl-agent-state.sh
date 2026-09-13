#!/bin/sh
# installed by herdl
# managed by herdl; reinstalling or updating the integration overwrites this file.
# add custom hooks beside this file instead of editing it.
# HERDL_INTEGRATION_ID=mastracode
# HERDL_INTEGRATION_VERSION=3

set -eu

action="${1:-}"
hook_input_file="$(mktemp "${TMPDIR:-/tmp}/herdl-mastracode-hook.XXXXXX")" || exit 0
trap 'rm -f "$hook_input_file"' EXIT HUP INT TERM
cat >"$hook_input_file" 2>/dev/null || true

case "$action" in
  session|working|idle|blocked) ;;
  *) exit 0 ;;
esac

[ "${HERDL_ENV:-}" = "1" ] || exit 0
[ -n "${HERDL_SOCKET_PATH:-}" ] || exit 0
[ -n "${HERDL_TAB_ID:-${HERDL_PANE_ID:-}}" ] || exit 0
command -v python3 >/dev/null 2>&1 || exit 0

HERDL_ACTION="$action" HERDL_HOOK_INPUT_FILE="$hook_input_file" python3 - <<'PY'
import json
import os
import random
import socket
import time

source = "herdl:mastracode"
action = os.environ.get("HERDL_ACTION", "")
pane_id = os.environ.get("HERDL_TAB_ID") or os.environ.get("HERDL_PANE_ID")
socket_path = os.environ.get("HERDL_SOCKET_PATH")
hook_input_file = os.environ.get("HERDL_HOOK_INPUT_FILE")

if not pane_id or not socket_path:
    raise SystemExit(0)

hook_input = {}
if hook_input_file:
    try:
        with open(hook_input_file, encoding="utf-8") as handle:
            content = handle.read()
        if content.strip():
            hook_input = json.loads(content)
    except Exception:
        hook_input = {}

request_id = f"{source}:{int(time.time() * 1000)}:{random.randrange(1_000_000):06d}"
report_seq = time.time_ns()
session_id = hook_input.get("session_id")
if isinstance(session_id, str) and session_id:
    agent_session_id = session_id
else:
    agent_session_id = None
if action == "session":
    if not agent_session_id:
        raise SystemExit(0)
    request = {
        "id": request_id,
        "method": "pane.report_agent_session",
        "params": {
            "pane_id": pane_id,
            "source": source,
            "agent": "mastracode",
            "agent_session_id": agent_session_id,
            "session_start_source": "startup",
            "seq": report_seq,
        },
    }
else:
    request = {
        "id": request_id,
        "method": "pane.report_agent",
        "params": {
            "pane_id": pane_id,
            "source": source,
            "agent": "mastracode",
            "state": action,
            "seq": report_seq,
        },
    }
    if agent_session_id:
        request["params"]["agent_session_id"] = agent_session_id

try:
    client = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    client.settimeout(0.5)
    client.connect(socket_path)
    client.sendall((json.dumps(request) + "\n").encode())
    try:
        client.recv(4096)
    except Exception:
        pass
    client.close()
except Exception:
    pass
PY
