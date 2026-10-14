#!/bin/bash
# A2: the switcher footer's settings gear really opens the App Switcher settings page on a real
# click, and that click does NOT commit the highlighted card.
#
# Why a real pointer gesture: the defect this scenario exists for was a gear that looked right and
# did nothing. Its mouseDown: override replaced NSButton's tracking loop without calling super, so
# no action was ever dispatched -- every geometry assertion passed and performClick: (which sends
# the action directly) passed too. Only a gesture through the window server reaches the broken
# path, so the assertion has to be a real click at the app's own reported gear rect.
#
# Why --show-overlay: the switcher normally appears only from the global hotkey, which steals
# focus and cannot run unattended. The development switch summons the same panel and leaves it
# standing, so the gesture works on the real view tree without touching the focused app.
#
# Exit code: 0 = every assertion passed; non-zero = failed, with the reason printed.
set -euo pipefail

repo_dir="$(cd "$(dirname "$0")/../.." && pwd)"

state="$(mktemp -t switcher-settings-state).json"
build_version=""
fail=0
launched=0

restore() {
    local status=$?
    cd "$repo_dir" || { echo "switcher-settings: cannot return to $repo_dir"; exit 1; }
    if [ "$launched" = 1 ]; then
        if ! scripts/dev-restart.sh --no-onboarding >/dev/null 2>&1; then
            echo "switcher-settings: the clean relaunch FAILED; the app may still be a --show-overlay instance"
            exit 1
        fi
    fi
    local pid
    for pid in $(pgrep -x oh-my-tab || true); do
        if ps -o command= -p "$pid" | tr ' ' '\n' | grep -qE '^--(show-overlay|e2e-state)'; then
            echo "switcher-settings: pid $pid is still running with a development switch after the restore"
            exit 1
        fi
    done
    rm -f "$state"
    exit "$status"
}
trap restore EXIT
trap 'exit 130' INT TERM

cd "$repo_dir"

command -v cua-driver >/dev/null 2>&1 || { echo "switcher-settings: cua-driver CLI not found in PATH"; exit 1; }
cua-driver status >/dev/null 2>&1 || { echo "switcher-settings: cua-driver daemon is not running"; exit 1; }

problem() { echo "switcher-settings: $1"; fail=1; }

cua() {  # <tool> <json args>
    python3 - "$@" <<'PY'
import subprocess, sys
completed = subprocess.run(["cua-driver", "call", sys.argv[1], sys.argv[2]], capture_output=True, text=True)
if completed.returncode != 0:
    raise SystemExit(f"cua-driver {sys.argv[1]} failed: {completed.stderr.strip()[:400]}")
print(completed.stdout)
PY
}

launch() {  # <label>
    local label="$1"
    local log
    log="$(mktemp -t switcher-settings-launch)"
    pkill -f "Oh-My-Tab-Dev.app" 2>/dev/null || true
    sleep 1
    echo '{}' > "$state"
    if ! scripts/dev-restart.sh --no-onboarding --show-overlay --e2e-state="$state" >"$log" 2>&1; then
        echo "switcher-settings: dev-restart.sh failed for the $label launch:"
        tail -n 20 "$log"
        exit 1
    fi
    launched=1
    [ -n "$build_version" ] || build_version="$(sed -n 's/^build-version: //p' "$log" | tail -1)"
    rm -f "$log"
}

wait_for_state() {  # <python expression over s>
    local expression="$1"
    local deadline=$((SECONDS + 25))
    while [ "$SECONDS" -lt "$deadline" ]; do
        if python3 - "$state" "$expression" <<'PY' 2>/dev/null
import json, sys
try:
    s = json.load(open(sys.argv[1]))
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

echo "switcher-settings: launching the overlay (--show-overlay)"
launch baseline
if ! wait_for_state "(s.get('switcher_panel') or {}).get('visible')"; then
    echo "switcher-settings: the overlay never reported a visible panel (--show-overlay produced no panel)"
    exit 1
fi

read -r panel_x panel_y panel_w panel_h gear_x gear_y gear_w gear_h clears < <(python3 - "$state" <<'PY'
import json, sys
s = json.load(open(sys.argv[1]))
p = s.get("switcher_panel") or {}
g = s.get("switcher_footer_settings") or {}
print(p.get("x", 0), p.get("y", 0), p.get("w", 0), p.get("h", 0),
      g.get("x", 0), g.get("y", 0), g.get("w", 0), g.get("h", 0),
      "yes" if g.get("clears_text") else "no")
PY
)

if python3 -c 'import sys; sys.exit(0 if float(sys.argv[1]) <= 0 or float(sys.argv[2]) <= 0 else 1)' "$gear_w" "$gear_h"; then
    echo "switcher-settings: the state carries no usable gear rect (panel ${panel_w}x${panel_h}, gear ${gear_w}x${gear_h})"
    exit 1
fi
if [ "$clears" != "yes" ]; then
    problem "the footer gear overlaps the status text (switcher_footer_settings.clears_text is false)"
fi

scale="$(cua get_screen_size '{}' | python3 -c 'import json,sys; print(json.load(sys.stdin)["scale_factor"])')"

# The click point in desktop pixels: the centre of the gear's real rect, which the app reports in
# screen points with a top-left origin -- the same space cua's desktop scope uses, so the only
# conversion is the display scale.
read -r click_x click_y < <(python3 - "$gear_x" "$gear_y" "$gear_w" "$gear_h" "$scale" <<'PY'
import sys
gx, gy, gw, gh, scale = (float(v) for v in sys.argv[1:6])
print(round((gx + gw / 2.0) * scale, 1), round((gy + gh / 2.0) * scale, 1))
PY
)

commits_before="$(python3 - "$state" <<'PY'
import json, sys
print((json.load(open(sys.argv[1])).get("last_commit") or {}).get("count", 0))
PY
)"
echo "  panel (${panel_x},${panel_y}) ${panel_w}x${panel_h}; gear ${gear_w}x${gear_h} at (${gear_x},${gear_y})"
echo "  clicking the gear in desktop pixels at (${click_x},${click_y}); commits so far ${commits_before}"

cua click "{\"scope\": \"desktop\", \"x\": $click_x, \"y\": $click_y, \"delivery_mode\": \"background\"}" >/dev/null

# The assertion the broken gear failed: the settings window must actually come up on its page.
if ! wait_for_state "s.get('settings_window_visible') and s.get('selected_sidebar') == 1"; then
    page="$(python3 - "$state" <<'PY'
import json, sys
s = json.load(open(sys.argv[1]))
print("visible" if s.get("settings_window_visible") else "not visible", s.get("selected_sidebar"))
PY
)"
    problem "clicking the gear did not open the settings window on the App Switcher page (page: ${page})"
else
    presented="$(python3 - "$state" <<'PY'
import json, sys
print(json.load(open(sys.argv[1])).get("settings_presented_page"))
PY
)"
    echo "  settings opened: selected_sidebar=1, presented_page=${presented}"
    if [ "$presented" != "1" ] && [ "$presented" != "None" ]; then
        problem "the settings window first appeared on page ${presented}, not the App Switcher page"
    fi
fi

# The click must not also commit the highlighted card: the gear is an exit, not a launch of the
# selected window. A stale mouseUp or a missing session retirement would show up here.
commits_after="$(python3 - "$state" <<'PY'
import json, sys
print((json.load(open(sys.argv[1])).get("last_commit") or {}).get("count", 0))
PY
)"
echo "  commits after the click: ${commits_after}"
if [ "$commits_after" != "$commits_before" ]; then
    problem "the settings click committed a card (commit count ${commits_before} -> ${commits_after})"
fi

echo
if [ "$fail" = 0 ]; then
    echo "switcher-settings: PASS (build ${build_version:-unknown})"
else
    echo "switcher-settings: FAIL (build ${build_version:-unknown})"
fi
exit "$fail"
