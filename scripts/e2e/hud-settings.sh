#!/bin/bash
# A2: the key panel's trailing settings gear really opens the Keystroke Display settings page on a
# real click, and the panel closes.
#
# Why a real pointer gesture: the same class of defect the switcher gear had -- a mouseDown:
# override that replaces NSButton's tracking loop without calling super leaves a control that looks
# right and dispatches nothing. Geometry assertions cannot see it; a gesture through the window
# server does. The click targets the gear's own rect as the app reports it.
#
# Why --show-keystroke-display: the panel otherwise appears only from a key event, which a script
# cannot produce without stealing focus. The development switch seeds the same panel through the
# same input queue the tap uses, and it is nonactivating, so nothing is focused by this run.
#
# Exit code: 0 = every assertion passed; non-zero = failed, with the reason printed.
set -euo pipefail

repo_dir="$(cd "$(dirname "$0")/../.." && pwd)"

state="$(mktemp -t hud-settings-state).json"
build_version=""
fail=0
launched=0

restore() {
    local status=$?
    cd "$repo_dir" || { echo "hud-settings: cannot return to $repo_dir"; exit 1; }
    if [ "$launched" = 1 ]; then
        if ! scripts/dev-restart.sh --no-onboarding >/dev/null 2>&1; then
            echo "hud-settings: the clean relaunch FAILED; the app may still be a development instance"
            exit 1
        fi
    fi
    local pid
    for pid in $(pgrep -x oh-my-tab || true); do
        if ps -o command= -p "$pid" | tr ' ' '\n' | grep -qE '^--(show-keystroke-display|e2e-state|keystroke-display-force-on)'; then
            echo "hud-settings: pid $pid is still running with a development switch after the restore"
            exit 1
        fi
    done
    rm -f "$state"
    exit "$status"
}
trap restore EXIT
trap 'exit 130' INT TERM

cd "$repo_dir"

command -v cua-driver >/dev/null 2>&1 || { echo "hud-settings: cua-driver CLI not found in PATH"; exit 1; }
cua-driver status >/dev/null 2>&1 || { echo "hud-settings: cua-driver daemon is not running"; exit 1; }

problem() { echo "hud-settings: $1"; fail=1; }

cua() {
    python3 - "$@" <<'PY'
import subprocess, sys
completed = subprocess.run(["cua-driver", "call", sys.argv[1], sys.argv[2]], capture_output=True, text=True)
if completed.returncode != 0:
    raise SystemExit(f"cua-driver {sys.argv[1]} failed: {completed.stderr.strip()[:400]}")
print(completed.stdout)
PY
}

launch() {
    local label="$1"
    local log
    log="$(mktemp -t hud-settings-launch)"
    pkill -f "Oh-My-Tab-Dev.app" 2>/dev/null || true
    sleep 1
    echo '{}' > "$state"
    # keystroke-display-force-on keeps the module active even when the user's config disables it,
    # so the scenario does not depend on this machine's settings.
    if ! scripts/dev-restart.sh --no-onboarding --show-keystroke-display --keystroke-display-force-on --e2e-state="$state" >"$log" 2>&1; then
        echo "hud-settings: dev-restart.sh failed for the $label launch:"
        tail -n 20 "$log"
        exit 1
    fi
    launched=1
    [ -n "$build_version" ] || build_version="$(sed -n 's/^build-version: //p' "$log" | tail -1)"
    rm -f "$log"
}

wait_for_state() {
    local expression="$1"
    local deadline=$((SECONDS + 25))
    while [ "$SECONDS" -lt "$deadline" ]; do
        if python3 - "$state" "$expression" "$repo_dir/scripts/e2e/lib" <<'PY' 2>/dev/null
import json, sys
# A duplicated member would make the gate read a different value than the app wrote,
# so the state is parsed by the duplicate-rejecting loader (see lib/state_json.py).
sys.path.insert(0, sys.argv[3])
from state_json import load_state
try:
    with open(sys.argv[1]) as handle:
        s = load_state(handle)
except Exception:
    raise SystemExit(1)
raise SystemExit(0 if eval(sys.argv[2], {"s": s}) else 1)
PY
        then
            return 0
        fi
        sleep 0.5
    done
    return 1
}

echo "hud-settings: launching the key panel (--show-keystroke-display)"
launch baseline
if ! wait_for_state "(s.get('keystroke_display') or {}).get('visible') and ((s.get('keystroke_display') or {}).get('geometry'))"; then
    echo "hud-settings: the key panel never reported a visible panel with geometry"
    exit 1
fi

read -r gear_x gear_y gear_w gear_h panel_w panel_h < <(python3 - "$state" <<'PY'
import json, sys
g = (json.load(open(sys.argv[1])).get("keystroke_display") or {}).get("geometry") or {}
s = g.get("settings") or {}
print(s.get("x", 0), s.get("y", 0), s.get("w", 0), s.get("h", 0), g.get("w", 0), g.get("h", 0))
PY
)

if python3 -c 'import sys; sys.exit(0 if float(sys.argv[1]) <= 0 or float(sys.argv[2]) <= 0 else 1)' "$gear_w" "$gear_h"; then
    echo "hud-settings: the state carries no usable gear rect (panel ${panel_w}x${panel_h}, gear ${gear_w}x${gear_h})"
    exit 1
fi

scale="$(cua get_screen_size '{}' | python3 -c 'import json,sys; print(json.load(sys.stdin)["scale_factor"])')"
read -r click_x click_y < <(python3 - "$gear_x" "$gear_y" "$gear_w" "$gear_h" "$scale" <<'PY'
import sys
gx, gy, gw, gh, scale = (float(v) for v in sys.argv[1:6])
print(round((gx + gw / 2.0) * scale, 1), round((gy + gh / 2.0) * scale, 1))
PY
)

echo "  panel ${panel_w}x${panel_h}; gear ${gear_w}x${gear_h} at (${gear_x},${gear_y})"
# The panel only accepts mouse input while its pointer poll sees the cursor over a control, and
# the poll runs on the panel own 16ms timer. Move onto the gear first and give the poll a beat,
# exactly as a user hand does on the way to a click; clicking from a standing start arrives
# before the panel has enabled input and the gesture is lost.
cua move_cursor "{\"scope\": \"desktop\", \"x\": $click_x, \"y\": $click_y}" >/dev/null || true
sleep 0.4
echo "  clicking the gear in desktop pixels at (${click_x},${click_y})"
cua click "{\"scope\": \"desktop\", \"x\": $click_x, \"y\": $click_y, \"delivery_mode\": \"background\"}" >/dev/null

# The assertion the broken gear would fail: the settings window must come up on its page.
if ! wait_for_state "s.get('settings_window_visible') and s.get('selected_sidebar_name') == 'keystroke-display'"; then
    page="$(python3 - "$state" <<'PY'
import json, sys
s = json.load(open(sys.argv[1]))
print("visible" if s.get("settings_window_visible") else "not visible", s.get("selected_sidebar_name"))
PY
)"
    problem "clicking the gear did not open the settings window on the Keystroke Display page (page: ${page})"
else
    echo "  settings opened: selected_sidebar_name=keystroke-display"
fi

# The panel is a HUD with no session to commit; it must be gone once the click is handled.
if ! wait_for_state "not (s.get('keystroke_display') or {}).get('visible')"; then
    problem "the key panel stayed visible after its settings gear was clicked"
fi

echo
if [ "$fail" = 0 ]; then
    echo "hud-settings: PASS (build ${build_version:-unknown})"
else
    echo "hud-settings: FAIL (build ${build_version:-unknown})"
fi
exit "$fail"
