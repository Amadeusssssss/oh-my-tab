# Review backlog

Findings that were valid but out of scope under *Review budget and triage* (AGENTS.md), plus known gaps that
no round closed. Nothing here is a claim that the behaviour is correct — each entry is a decision to schedule
the work instead of doing it inside the change that surfaced it.

Add an entry when a round is triaged, not later: round, severity, why it is deferred, and the trigger that
brings it back. Remove an entry when its work lands, and say so in the commit or release note.

## Open

| # | Item | From | Severity | Why deferred | Revisit trigger |
| --- | --- | --- | --- | --- | --- |
| 1 | Cross-desktop minimized window: no A2 scenario asserts the restore path end to end | feature review, R9–R12 | MEDIUM | the path exists and is covered by A1/A2 for the current desktop; the cross-desktop variant needs a second desktop with a minimized window, which the scenario cannot set up deterministically | next change to `raise_other_desktop_window` or the minimized restore path |
| 2 | Same app with windows on two desktops: no A2 scenario | feature review, R9–R12 | MEDIUM | requires the app to own a window on each desktop; the frame-level `ax_published_pids`/`ax_recovered_pids` evidence only stops the existing scenario from mis-failing on it | next change to cross-desktop admission |
| 3 | A real second window whose desktop has not been visited since this process started stays hidden | WeChat duplicate fix | LOW (accepted trade-off) | hiding it is what kills the dead duplicate card; the window appears after its desktop is visited once | if a user reports a missing cross-desktop window |
| 4 | An app whose `kAXWindows` **and** key/main slots are both empty is refused outright, so a real window it owns on another desktop stays hidden | WeChat duplicate fix, narrowed again by the Stats fix | LOW (accepted trade-off) | in that state nothing tells a real window from a closed menu-bar panel, and both wrong answers are worse than hiding it: admitting the CG entries put Stats' closed "Combined modules" panel on screen as a dead card, while a failed *query* (a different state) keeps the fallback for windows that are visible | if a user reports a missing cross-desktop window for an app that answers AX with nothing at all |
| 6 | Synthetic Dock swipe for an instant, menu-bar-correct Space switch (BetterCmdTab) | improvement candidate | — (optimisation) | ported and measured 2026-10-07: the three-phase gesture (`CGEvent` fields 55/110/123/124/129/130/132) is created and posted successfully (`delivered=true`) both from a plain process and from inside the app process, and the Space does **not** move on macOS 26 in either case. The reference also installs a swipe-suppressor event tap and gates the feature behind its `instantSpaceSwitch` preference, so the event sequence alone is not the whole mechanism. The probe stays behind `--space-swipe=left|right` (`src/space_swipe.rs`) for the next attempt | if the animated cross-Space transition is reported as too slow, or when the suppressor-tap interaction is understood |
| 10 | Thumbnail card size still moves a ladder step when the window **count** crosses a threshold, and a set much wider than the base ratio can need a scroll row at the chosen size | user reports, 2026-10-07 | LOW (recorded trade-off) | the user chose to keep the current behaviour for now and have it documented: `docs/design-style{,-en}.md` §9 states the policy and `docs/developer-notes{,-en}.md` the mechanism, including the reproducible log line | when the size jump is reported again, or on the next overlay-layout change: (a) prefer the largest step whose overflow is at most one teaser row before stepping down, and (b) smooth the count thresholds (more steps, or a row-count anchor). **(c) is done**: the user-pinned Thumbnail size dropdown (2026-10-07) removed the order dependence, and `auto` now estimates rows from the count and the widest card only |
| 18 | A fullscreen window **on another desktop** has no end-to-end card assertion (its `fullscreen` flag and `window_server` source) | state-decode review, R1–R3 (this session) | LOW (coverage gap) | the cross-desktop state assertions now land in `scripts/e2e/space-desktops.sh` (minimized/fullscreen/sources/`ordered_in`/`ax_pairing`/raw row fields, PASS 2026-10-08) and the `0x20` Space mask was verified on this machine by toggling `AXFullScreen` on an off-desktop window; what is still missing is a scenario that puts a window in a fullscreen Space *and* asserts its card's fullscreen flag (this app cannot fullscreen its own probe window, so the scenario would have to drive another app) | next change to `space-desktops.sh` or `window_state.rs` |
| 19 | The A2 scenarios disagree about the switcher chord: `space-desktops.sh` now derives it from `keyboard.modifier`, but `tab-repeat.sh` hard-codes Command+Shift+Tab and `switch-basic.sh` defaults to `cmd+tab` instead of reading the config | cross-desktop state review, 2026-10-08 | MEDIUM (instrument) | both scenarios pass on a Command-configured machine (this one), and the shared `scripts/e2e/lib/hotkey.py` regression cases already cover the mapping, so this is a consistency fix rather than a broken gate | next change to either scenario, or the first time the suite runs on a machine configured with `option` |
| 11 | No A-tier coverage for a settings control writing the config (any dropdown, including Thumbnail size) | this session | LOW (coverage gap) | the app's custom select needs a real mouse-down to open its menu, and no scenario drives one; the read direction (config -> popup) and the mapping are covered, the write direction is code-reviewed only | when a smoke can drive a control with persistence suppressed, or when one of these writes breaks |
| 16 | The keystroke display HUD keeps its native window shadow instead of the shared elevation carrier | panel outline/elevation change, 2026-10-07 | MEDIUM (recorded decision) | it is placed `PANEL_EDGE_MARGIN` (18pt) from the visible edge, and a `high`-level shadow needs 64-76pt of window padding; measured, AppKit then constrains the padded window back into the visible area and the panel moves by 26pt, which also changes what a saved `keystroke_display.position` means. Giving the HUD a shadow is a placement decision of its own (move it inward, or pad per-edge against the screen edge it sits on), not a second elevation level | next change to the keystroke display's placement or saved position, or when the HUD's native shadow is reported as the dark ring the other panels turned off |
| 17 | `liquid-glass` panel outline is drawn by the shared decoration view, so it coexists with the glass's own edge highlight | panel outline/elevation change, 2026-10-07 | LOW (to watch) | measured on the picker: the stroke is 1pt at the `card_border` tone on the panel's own edge and nothing else changed; a double edge on `.regular` glass has not been reported or measured as a defect. The document's §3.4 states the outline is one visual contract across materials, so a material-specific exception would have to be a documented one | if the glass edge reads as two rims, or on the next change to the glass style/tint |
| 12 | ~~A fullscreen window whose origin is the current desktop is missing from that desktop's cards~~ **CLOSED 2026-10-07** by the location-based contract: every fullscreen Space's window is admitted whatever the switch says, so neither a missing origin nor the CG-only gate's old `OtherDesktop` requirement can hide it (`space-desktops.sh` pins it: "with the switch off a fullscreen window is still a card") |
| 13 | `scripts/e2e/space-groups.sh` cannot run on this machine: it needs a bound "move left/right a space" shortcut for its `ctrl+left` step, and keys 79/80 are enabled with no keys assigned | this session | LOW (environment) | the scenario is a focus scenario (skipped by `run-all.sh` unless `--include-focus`); the origin logic it covers is asserted by A1 unit tests, and its fullscreen phase was exercised once (it learned the origin and listed the fullscreen window) | when a Space-switch shortcut exists, or when the scenario is reworked to switch Spaces by another means |
| 14 | `scripts/e2e/space-desktops.sh` picks its cross-desktop target by heuristic, so it can land on a window the app cannot admit yet (the documented "desktop not visited in this process" narrowing) and report an unrun phase | this session | LOW (instrument) | the scenario now filters to an ordinary other desktop whose app has no window here, and reports NOT RUN with the admitted cards instead of a false FAIL; picking the target from the admitted other-desktop cards themselves needs a scenario restructure | when the scenario is next touched, or when a target choice hides a real admission regression |
| 15 | ~~The fullscreen origin is inferred from the current Space adjacency, so a reordered Space list groups that Space with the wrong desktop~~ **CLOSED 2026-10-07**: the origin no longer takes part in admission (the contract reads the window's location), so a wrong association can no longer hide or misplace a card; the inference stays as an e2e diagnostic, with `a_reordered_space_list_groups_by_adjacency` still pinning what it reports |
| 9 | `space-desktops.sh`'s capture-eligibility check reports NOT RUN when the summon frame arrives before the thumbnail workset is published | this session | LOW (instrument) | the check is honest (NOT RUN, never a false PASS) and a re-run passes; making it wait for a published workset is instrument polish | next change to that scenario |
| 8 | Config-change in-flight invalidation has no multi-thread interleaving assertion | feature review, R9 | LOW | the behaviour is asserted end to end (A2 switch off/on); the interleaving needs a deterministic hook | next change to `runtime_config` invalidation |
| 21 | A `--smoke-*` launch leaves its per-process scratch directory (`$TMPDIR/oh-my-tab-clip-images-smoke-<pid>`) behind: the runners exit through `process::exit()` and nothing removes it | smoke isolation task, review round 2, 2026-10-11 | LOW (scratch) | since the pasteboard gate a smoke launch can only write its own synthetic entries, so the directories are junk rather than user content, and the reviewer ranked automatic cleanup as non-blocking; a cleanup that must run on every `process::exit()` path (20 runners plus crash paths) would be a new instrument to get wrong | next change to `clipboard::smoke` or `monitor::start`, or if scratch accumulation is reported (measured 2026-10-11: 60 directories, 30 MB, including pre-fix runs that had recorded real pasteboard content — deleted by hand) |
| 22 | The picker header's OTHER static inks (search field, corner close button, filter pills, footer legends) are not repainted when the theme changes while the picker window is reused | clipboard gear/drag review, R1 (finding 6) | LOW | the gear and the drag handle were fixed in `apply_theme` because that change added them; the rest predate it and repainting the whole header is its own change (the header is built once and only the footer and the clear actions are rebuilt today) | next change to the picker header's styling, or the first report of a stale header colour after a light/dark switch |
| 23 | `scripts/e2e/panel-edge.sh` fails on this machine (checks 1–3 are screenshot-based outline/shadow/material-blur measurements), and it fails the same way at HEAD, so the default A2 aggregate currently reports one FAIL that is not attributable to the change under test | clipboard gear/drag session, 2026-10-12 | MEDIUM (gate) | the failure is environmental and predates this work: verified in a clean `git worktree` of HEAD, where the same three checks fail with identical output while every other scenario passes. Diagnosing it needs a display/backdrop setup the current session does not control (the checks compare a white/gray backdrop capture), which is its own task | next time the panel outline/elevation or the backdrop capture path changes, or when a machine with a known-good capture setup can run it |
| 24 | The storage notice's action-per-state rule, the purge transaction's completion boundary, and the purge's conservative verification have no A-tier assertion | clipboard blocked-notice task, design rounds 1–6 + implementation, review rounds 1–3, 2026-10-12 | MEDIUM (coverage gap) | the user explicitly required no new tests in this round, and the reviewer confirmed that exception is the user's to give (not the reviewer's) — the fixes themselves were not deferred. What exists instead: the classifier is pure (`notifications::storage_action_key`), each surface reports the action it ACTUALLY applied in `--e2e-state`'s `clipboard_notice` (`grant`/`clear`/`hidden`/`absent`/`unset`), and the existing smokes only cover the `Unavailable` path's button title/selector (`--smoke-clipboard`) and the About row's repaint in `Ready` | next change to `clipboard/notifications.rs`, the purge transaction (`clipboard/storage.rs`, `clipboard/persist.rs`), the settings banner or the picker empty state — or the first report of a wrong or dead action. Cheapest assertions: (a) pure — `ClearAndRestart` for the four blocked reasons, `GrantKeychain` only for `Unavailable`, `None` for `PurgePending`/`Ready`/`Uninitialized`; (b) smoke — park a blocked state, then assert the panel button's title+selector and the banner's `clipboard_notice` value; (c) completion boundary — capture a load outcome with the generation a purge's first bump produced, then assert `apply_load_outcome` drops it after the boundary advanced the generation (the `apply_stale_load_for_tests` shape already exists); (d) purge verification — a surviving regular file and an unreadable cache entry each keep the transaction incomplete, while a confirmed non-file object (the test-mode `history` subdirectory) does not; (e) the sealed-cache probe — an unreadable entry or a symlink answers "unknown" (no key creation, and no auto-isolation: `Blocked`, not the permanently-lost-key path) |
| 25 | Key bindings, phase 2: editing an existing row's chord in place (改键), parameter-bearing actions (open an app / a folder / a URL / a Settings pane), desktop switching, window restore, the press/release two-phase switcher action, and grouping the action picker (the self-drawn popup has no separator items) | key-bindings feature, design rounds 1–3 + implementation review rounds 1–3, 2026-10-12 | — (scope) | the user froze phase 1 to parameter-free actions and deferred 改键; deleting a default row and adding a new one already reaches the same result, and Restore Defaults undoes it. Desktop switching needs a working mechanism (the synthetic Dock swipe is a probe that does not move the Space on macOS 26) and restore needs per-window pre-snap state that nothing persists today | next change to `src/key_bindings/`, or when the user asks for phase 2 |
| 26 | No A2 scenario drives a user binding's record → confirm → execute path end to end (the switcher and clipboard hotkeys have their own scenarios, but a binding recorded in the settings UI does not) | key-bindings implementation, review round 3, 2026-10-12 | MEDIUM (coverage gap) | such a scenario must inject a global chord through the system event stream and therefore steal focus, so it needs the user's consent every run; the recorder's decisions are asserted in A1 instead (`release_wait_step`'s full sequence, `draft_rejection`, the queue's trigger re-authorization, the migration and fallback matrix), and `scripts/e2e/settings-layout.sh` asserts the page's geometry, the sidebar entry and the published-state invariants with counter-examples for a snapshot missing those fields. Explicitly unverified in a GUI: that a cancelled recording restores the add row, that a draft is cleared by a window close or a permission revocation, and that leaving the page cancels — the state behind each is asserted in A1 (`has_draft`/`pending_binding_recording`, the unconditional main-thread cleanup), but no scenario drives the interaction | next change to the recorder or the add row, or the first report of a recorded binding not firing, a stuck add row, or a stale draft |
| 27 | `scripts/e2e/settings-layout.sh` has not been run since the Key Bindings page was merged into one flat list, so its geometry assertions (one card covers its row, the card bottom follows the live row height) and the migrated-state invariant (`key_bindings_live: 5, key_bindings_entries: 13, key_bindings_enabled: true` for a mixed legacy config) are verified only in part | key-bindings implementation, review round 12, 2026-10-12 | MEDIUM (coverage gap) | the scenario drives the real window and steals focus, so it needs the user's consent per run. What *is* verified: `binding_state_problem`'s counter-examples pass standalone (including the `live=5 / entries=13 / enabled=true` snapshot that the corrected mixed-group migration now produces, checked on the user's real config — `[key_bindings] published 5 live binding(s) from 13 entr(ies)`, down from 13 before the fix), the card-coverage debug assertion is exercised by all seven settings pages plus the 1/18/0-binding variants under `scripts/dev-restart.sh` with zero coverage or panic lines, and the `live_row_height` regression is pinned by A1 tests | next change to the Key Bindings page layout or `page_canvas.rs`, or the next A2 run with focus consent |

## Closed in the same session (do not reopen)

The rounds listed below were all fixed in-session before the budget policy existed; they are recorded here only
so a later reader does not re-derive them. The fixes are in the tree, with their A1 counter-examples.

- Item 20 (the `shipped_release_notes_satisfy_the_renderer` gate): **the check was removed on 2026-10-12 at the
  user's direction**, after the choice was put to them (strip the inline markdown from the historical notes, or
  narrow the scan to the shipped version). `release_doc_dev/0.2.2.md` (en) and `0.2.3.md` (all three locales)
  still carry `**`, which `render_release_notes_markdown` shows literally in the About page's notes; nothing
  asserts that any more, so a note that introduces inline markdown, or drops a locale block, is caught by review
  only. `cargo test` is green again (855 passed / 0 failed).
- R12–R18: ownership of the staged eligibility evidence (per-pass staging, `FORGET_EPOCH`, prewarm paths).
- R21–R22: the card render decision (`thumbnail_rendered`), the exact-focus check ordering, and the normal-path
  branch assertion in `scripts/e2e/space-desktops.sh`.
- R24–R28: AX-identity history for cross-desktop admission (per-thread evidence staging, counted pass epochs,
  lifecycle veto, mark reclamation).
- Origin-Space front process (was item 5 here): **not a defect on this machine.** Measured 2026-10-07: a
  cross-Space commit followed by a return through `SLSManagedDisplaySetCurrentSpace` left the origin Space
  showing the app that was frontmost before the switch, and the user then ran the real scenario -- switch
  away with the switcher, return with a four-finger horizontal swipe -- and the front app was correct. The
  reference implementation repairs this with `SLSSpaceSetFrontPSN` because it fronts apps through
  `_SLPSSetFrontProcessWithOptions`; our activation path does not clobber it, and the SLPS *rescue* path
  (which shares that call) was not exercised in either test because activation landed the switch. If a
  report of "returning to a desktop shows the wrong front app" ever arrives, the fix is prepared: snapshot
  `NSWorkspace.frontmostApplication` and the origin Space id before the front-switch, then call
  `SLSSpaceSetFrontPSN` afterwards, only when the target's cross-Space membership is confirmed and the
  origin's front was not us (the guard that avoids undoing the raise, AltTab #5586).
- `scripts/e2e/settings-layout.sh` (was item 7 here): the chronic 33/44 failure was one stale constant
  (`TOP_PADDING = 50` vs the normative 48) plus three more expectations left behind by the same token
  migration (in-row button height 28 -> 32, row-to-row 62 -> 60 = 52 + 8, card-to-header 77 -> 72.2 =
  24 + 4 + 20 + 16.2 + 8). The 58.7 step that looked off-grid is the same row pitch measured through a
  row's value label, which sits 1.3pt above its row label. Repaired to 44/44 with no tolerance widened.

- The terminal classification cannot read the system frontmost app yet. `NSWorkspace.frontmostApplication`
  belongs to the main thread and `terminal_evidence` runs on `ax-raiser`, so the read is gone and
  `frontmost_pid_matches` is `None`: while the raise experiment carries `--activation-api` the terminal is
  therefore `unknown` rather than `landed`, and the product path records no terminal at all (`terminal=-`,
  measured 2026-10-08, `scripts/e2e/space-desktops.sh --include-focus` still PASS). Deferred because the
  honest fix is the main-thread verification channel the phase-2 repair step has to build anyway, and
  keeping a background AppKit call alive for a diagnostic is exactly the breach the invariant forbids.
  Trigger: the change that adds that channel (then sample the frontmost pid on the main thread and carry it
  back as plain data), or any move of the terminal into product behaviour.

- `setEnabled:` alone leaves a **layer-filled `SettingsButton` looking active**: the styled button
  draws its own layer fill, which `setEnabled:` does not dim, so a disabled primary button keeps its
  full accent and reads as pressable (hit-testing is still correct; only the appearance lies). Found
  while building the Key Bindings add panel's OK button, which now dims itself at half opacity
  locally (`apply_panel_button_dim`), matching the style doc's disabled rule (50% opacity on the
  control's fill). Deferred because the honest fix belongs in
  `SettingsRow::set_view_enabled_with_tooltip` — one branch for styled buttons — and that changes the
  appearance of every disabled button in the app, which is its own visual review rather than
  something to smuggle into this change. Trigger: the next change to `set_view_enabled_with_tooltip`
  or `SettingsButtonRole`, a visual pass over disabled controls, or the first report (screenshot) of a
  disabled button that looks enabled.

- The **styled-button disabled appearance** is only fixed locally. `setEnabled:` does not dim a
  layer-filled `SettingsButton`, so a disabled button keeps its full fill and reads as pressable
  (hit-testing is correct; only the appearance lies). The Key Bindings add panel dims its own two
  buttons at half opacity (`apply_panel_button_dim`, the style doc's §10 rule), but every other
  disabled styled button in the app still shows its normal fill. A general fix belongs in
  `SettingsRow::set_view_enabled_with_tooltip` (one branch for styled buttons) and changes the
  appearance of every disabled button, which is its own visual pass rather than something to fold
  into a layout change. Trigger: the next change to `set_view_enabled_with_tooltip` or
  `SettingsButtonRole`, a visual review of disabled controls, or a report (screenshot) of a disabled
  button that looks enabled.

- Recovery-coverage evidence for the cross-desktop raise (raised in review round 2 as MEDIUM: deleting the
  pre-action wait also shortened the recovery coverage). Three states still lack a counter-example: a first
  AX raise that matches no element, a target that only arrives late, and a cross-desktop *minimized* restore.
  Deferred because each needs a purpose-built scenario -- an element absent on the first attempt, a delayed
  arrival, a minimized window parked on another desktop -- and nothing can force those states from outside
  today. What *is* covered: `scripts/e2e/space-desktops.sh --include-focus` drives the rescue branch end to
  end and now reconciles the system frontmost pid **and** the exact focused window (phase 4), and
  `--other-desktop-no-activation` makes the suppressed-activation branch deterministic
  (measured 2026-10-08: `activation=false`, `first_rescue_ms=0`, `elapsed_ms=216`, target app frontmost,
  focused window == the selected one).
  Trigger: the next change to the raise or recovery path, or the first report of a delayed or minimized
  cross-desktop switch that does not land.

- The notification **click** path has no assertion. `did_receive_notification_response` (the
  `didReceiveNotificationResponse:withCompletionHandler:` delegate method) is exercised only when a user
  clicks an update or clipboard notice, and it cannot be driven from a smoke runner: the callback reads
  `response.notification.request.identifier`, and `UNNotificationResponse` has no public initializer to
  build a fake one from. Its arity is correct -- three explicit arguments, unlike the `willPresent`
  callback that crashed on 2026-10-08 because it was one argument short and therefore read the
  notification object as the completion handler -- and its zero-argument completion block uses the same
  hand-rolled invoke the module's authorization block already runs successfully. Deferred as unrun
  coverage. Trigger: any change to the click path, or a report that clicking a notice does nothing.

- The keychain prompt is **solved and the mechanism measured** (2026-10-08): the ACL is judged against the
  app that *created* the item, so an item created by an ad-hoc build makes every later build a stranger
  (`SecItemDelete` on it is refused with `errSecInvalidOwnerEdit`), while an item created by the Developer
  ID-signed build survives rebuilds (probe and real app: 67 ms, no prompt). The dev bundle now signs with
  Developer ID and the one-time reset -- user deletes the item, the app creates a fresh one and keeps the
  old storage aside as `*.failed-<ts>` -- was performed on this machine. No backlog work is pending here;
  what remains unverified is a *release* install that predates Developer ID signing, which would need the
  same one-time reset (the release channel does sign with Developer ID today).

- **The whole-row selection overlay must stay below the row's controls** (2026-10-13). Moving the
  binding row onto the shared builders put the overlay on top (`addSubview:` appends, and the end is
  the top), so the transparent overlay swallowed the switch's clicks and clicking the switch selected
  the row instead. Fixed with `addSubview:positioned:relativeTo:` at index -1, and
  `debug_validate_row_overlay_is_below_controls` (A1, via `--smoke-settings-layout`) asserts the
  ordering; falsified by restoring the plain `addSubview:` (reports overlay index 5 vs switch 4).
  Note for the next reader: a `hitTest:`-based check was tried first and was **not** usable from a
  smoke runner -- it returns an unrelated view depending on window coordinate state. Subview order is
  the deterministic fact.

- **The key-bindings footer's segment gating has one A1 assertion, not an A2 one** (2026-10-13).
  `--smoke-settings-layout` drives `update_binding_controls_enabled` and asserts both segment flags
  against the page state (falsified: removing the `-` selection gate makes it report
  `remove=true (want false)`). What is *not* asserted is the end-to-end route: an A2 scenario that
  selects a row, clicks the `-` cell and watches `entries` fall. That was verified by hand this round
  (`selected row 1` -> `removing selected row 1` -> `2 entr(ies)`), and `+` -> `binding add panel
  opened`, but tier-B evidence is not an assertion. Deferred as unrun coverage. Trigger: the next
  change to the footer control, or `scripts/e2e/settings-layout.sh` gaining a key-bindings section.

- **`NSControl::setEnabled:` on a segmented control deserves a comment, not a rule** (2026-10-13).
  The first cut of the joined footer control ordered its whole-control `setEnabled:` after the
  per-segment calls, on the theory that `NSControl`'s setter resets every segment. Measured against
  the real control (falsified both orders through `--smoke-settings-layout`): it does **not** -- both
  orders produce identical per-segment flags, so the original ordering was never a bug. The comments
  now record the measurement instead of the theory. No further work; recorded so the wrong theory is
  not "fixed" again from the AppKit header alone.
