#!/usr/bin/env bash
# Dev entry for the desktop shell: `npm run desktop:dev` → `tauri dev`
# runs this as `beforeDevCommand`. If something already serves the dev URL
# (e.g. `npm run dev` in another terminal), reuse it instead of crashing on
# the busy port — but only when it is actually our app.
set -euo pipefail

DEV_URL="http://localhost:1420/"

if curl -sf --max-time 3 "$DEV_URL" 2>/dev/null | grep -qi "<title>vicuna</title>"; then
  echo "dev server already running on :1420 — reusing it."
  echo "(stop that server separately; Ctrl+C here only stops the app shell)"
  exec tail -f /dev/null
fi
if curl -sf -o /dev/null --max-time 3 "$DEV_URL" 2>/dev/null; then
  echo "error: port 1420 is already in use by something that is not Vicuna." >&2
  echo "free the port, then retry." >&2
  exit 1
fi
exec npm run dev
