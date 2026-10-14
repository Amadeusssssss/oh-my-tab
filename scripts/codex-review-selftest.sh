#!/usr/bin/env bash
#
# Self-test for scripts/codex-review.sh.
#
# The helper is run against a stub `codex` in a throwaway git repository, so no reviewer session and
# no network are involved. It covers the failure modes the helper exists to classify and the
# behaviour it promises around them:
#
#   - a run that succeeds, and one whose CLI exits non-zero
#   - a run that exits 0 without a completed turn (a transport that died mid-answer)
#   - a completed turn with no final message, and one whose message is not a verdict
#   - a run that hangs, terminated by --timeout
#   - the --retry framing, which must not claim the code was updated
#   - a resume that fails, and the --fresh recovery from it
#   - the lock that keeps two reviews from interleaving
#   - the archive that keeps every attempt, including the failed ones
#   - a relative --note-file, and stdin closed so the CLI cannot read input
#
#   scripts/codex-review-selftest.sh
#
set -uo pipefail

script_dir="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
helper="$script_dir/codex-review.sh"

[[ -f "$helper" ]] || { echo "cannot find $helper" >&2; exit 66; }

# The instrument controls its own environment. A task key the caller exported — a harness that says it once,
# as the helper's documentation suggests — would move every keyless case's state into that task's directory
# while the assertions still read the top-level paths, and the gate would fail for a reason that belongs to
# the caller's shell. The same holds for a model or speed override. The cases that test any of those
# variables set it on the call itself.
unset OMT_REVIEW_TASK OMT_REVIEW_MODEL OMT_REVIEW_FAST

tmp="$(mktemp -d)"
# The pre-check below asks whether the tree is clean, so the runs that test it must keep the harness's
# own output files outside the repository: shell redirection truncates them before the script starts,
# which would dirty the tree and make the check unsatisfiable.
scratch="$(mktemp -d)"
trap 'rm -rf "$tmp" "$scratch"' EXIT
mkdir -p "$tmp/scripts" "$tmp/bin"
cp "$helper" "$tmp/scripts/"
cd "$tmp"
git init -q .
git config user.email "selftest@example.invalid"
git config user.name "self-test"

cat > bin/codex <<'STUB'
#!/bin/bash
# A stand-in for the CLI: emits the event shapes the helper classifies, writes the -o file when it
# has a final message, and records what it was asked and what its stdin looked like.
mode="${STUB_MODE:-ok}"
prompt="${@: -1}"
printf '%s' "$prompt" > "$STUB_DIR/last-prompt.txt"
# The session id carries the task key the prompt named, so a round that resumed another task's
# session shows up in the assertion instead of looking like the right one. Read from the prompt the
# way the helper wrote it; a keyless round names no task.
task_key="$(printf '%s' "$prompt" | sed -n "s/.*belongs to task '\\([a-z0-9._-]*\\)'.*/\\1/p" | head -1)"
echo called >> "$STUB_DIR/calls.txt"
if read -t 1 _; then echo "data" > "$STUB_DIR/stdin.txt"; else echo "rc=$?" > "$STUB_DIR/stdin.txt"; fi

out=""; prev=""; is_resume=0; model=""; resume_id=""; tier=""
for a in "$@"; do
    [[ "$prev" == "-o" ]] && out="$a"
    [[ "$prev" == "--model" || "$prev" == "-m" ]] && model="$a"
    [[ "$a" == "resume" ]] && is_resume=1
    [[ "$prev" == "resume" ]] && resume_id="$a"
    # The helper passes the speed as `-c service_tier=<tier>`; recorded so a case can assert that the
    # requested tier reaches the CLI on the first round *and* on a resume, which is where it used to be lost.
    [[ "$prev" == "-c" && "$a" == service_tier=* ]] && tier="${a#service_tier=}"
    prev="$a"
done
printf '%s' "$model" > "$STUB_DIR/last-model.txt"
printf '%s' "$tier" > "$STUB_DIR/last-tier.txt"
printf '%s' "$is_resume" > "$STUB_DIR/last-resume.txt"
# The id it was actually told to resume, not only that it was a resume: the thread id below is built from
# the prompt, so a round that resumed another task's session would otherwise look like the right one.
printf '%s' "$resume_id" > "$STUB_DIR/last-resume-id.txt"

start() {
    if [[ -n "$task_key" ]]; then
        echo "{\"type\":\"thread.started\",\"thread_id\":\"stub-$task_key\"}"
    else
        echo '{"type":"thread.started","thread_id":"stub-session"}'
    fi
    echo '{"type":"turn.started"}'
}
# The real CLI's wording, verbatim, when it will not use the requested service tier: it reports an error
# item and answers at the default speed rather than failing the run. The helper keys its warning and its
# archived `service_tier_rejected` off this sentence. The tier named is the one actually requested, so a
# detection that matched a hard-coded value instead of the round's own would not be flattered by it.
tier_rejected_item() {
    python3 -c 'import json,sys; print(json.dumps({"type":"item.completed","item":{"type":"error","message":"Configured service tier `%s` is not advertised as supported for model `gpt-6.1-sol` and will be omitted from requests." % (sys.argv[1] or "none")}}, ensure_ascii=False))' "${tier:-}"
}
# An unrelated error item: the tier warning must not fire on every error the CLI prints.
unrelated_error_item() {
    python3 -c 'import json; print(json.dumps({"type":"item.completed","item":{"type":"error","message":"stream disconnected before completion"}}, ensure_ascii=False))'
}
# The sharper counter-example: the *same* generic phrase, about a different setting. A match that only
# looked for "not advertised as supported for model" would archive a service-tier rejection here.
other_setting_rejected_item() {
    python3 -c 'import json; print(json.dumps({"type":"item.completed","item":{"type":"error","message":"Configured reasoning effort `ultra` is not advertised as supported for model `gpt-6.1-sol` and will be omitted from requests."}}, ensure_ascii=False))'
}
msg() {
    [[ -n "$out" ]] && printf '%s\n' "$1" > "$out"
    python3 -c 'import json,sys; print(json.dumps({"type":"item.completed","item":{"type":"agent_message","text":sys.argv[1]}}, ensure_ascii=False))' "$1"
}
turn_done() { echo '{"type":"turn.completed","usage":{}}'; }
passing() { printf '审查结论：本轮通过，未发现显著问题。\n\nREVIEW-STATUS: PASS'; }

case "$mode" in
    ok)           start; msg "$(passing)"; turn_done ;;
    no-completed) start; msg "正在审查，先看" ;;
    no-message)   start; turn_done ;;
    not-verdict)  start; msg "需要我继续吗？"; turn_done ;;
    unfinished)   start; msg "$STUB_TEXT"; turn_done ;;
    fail)         start; echo "boom: transport reset" >&2; exit 1 ;;
    verdict-then-fail) start; msg "$(passing)"; echo "boom after answering" >&2; exit 1 ;;
    hang)         start; sleep 30 ;;
    ignore-term)  start; echo $$ > "$STUB_DIR/child.pid"; trap '' TERM; sleep 30 ;;
    child-survives) start
                  echo $$ > "$STUB_DIR/child.pid"
                  ( trap '' TERM; : > "$STUB_DIR/child-ready"; sleep 30 ) &
                  echo $! > "$STUB_DIR/grandchild.pid"
                  sleep 30 ;;
    late-session) trap '' TERM
                  : > "$STUB_DIR/late-ready"
                  ( while [[ ! -f "$STUB_DIR/late-go" ]]; do sleep 0.05; done
                    echo '{"type":"thread.started","thread_id":"late-session"}'
                    sleep 30 ) &
                  sleep 30 ;;
    events-only)  start; msg "$(passing)"; turn_done; : > "$out" ;;
    unclear)      start; msg "审查尚未完成，需要我继续吗？"; turn_done ;;
    plan)         start; msg "$(printf '方案可行，可以照着做。\n\nPLAN-STATUS: AGREED')"; turn_done ;;
    plan-fail)    start; echo "boom: the transport went away" >&2; exit 1 ;;
    resume-fail)  if [[ "$is_resume" == 1 ]]; then echo "session not found" >&2; exit 1; fi
                  start; msg "$(passing)"; turn_done ;;
    # The real CLI's wording when the thread it was told to resume no longer exists. The helper keys
    # the `lost` event off exactly this sentence, so the stub has to say it verbatim: a paraphrasing
    # stub would let a detection that no longer matches the CLI pass the gate.
    thread-gone)  if [[ "$is_resume" == 1 ]]; then
                      echo "Error: thread/resume: thread/resume failed: no rollout found for thread id $resume_id (code -32600)" >&2
                      exit 1
                  fi
                  start; msg "$(passing)"; turn_done ;;
    # A CLI that dies before it reports a thread.started: the attempt produces events and even a
    # verdict, but no session. Nothing about the round may be recorded as a session existing.
    no-session)   echo '{"type":"turn.started"}'; msg "$(passing)"; turn_done ;;
    # A completed, valid review whose requested speed codex declined: the round still passes, and the
    # warning plus the archived flag are the only evidence that fast mode did not take effect.
    tier-rejected) start; tier_rejected_item; msg "$(passing)"; turn_done ;;
    # The counter-example for that warning: an error item that is not about the service tier.
    error-item)   start; unrelated_error_item; msg "$(passing)"; turn_done ;;
    # The sharper counter-example: the same generic phrase about a *different* setting, which must not be
    # archived as a service-tier rejection.
    other-setting) start; other_setting_rejected_item; msg "$(passing)"; turn_done ;;
esac
exit 0
STUB
chmod +x bin/codex
export STUB_DIR="$tmp"

failures=0

run() {
    local mode="$1"; shift
    set +e
    STUB_MODE="$mode" PATH="$tmp/bin:$PATH" ./scripts/codex-review.sh "$@" >"$tmp/out.txt" 2>"$tmp/err.txt"
    echo $? > "$tmp/exit.txt"
    set -e
}

# Waits for a child to be ready *and* leaves behind a usable pid, reported through AS_PID. It must not
# be called in a command substitution: the failure would be recorded in the subshell and the error text
# would be captured as if it were a pid. So it returns a status, writes diagnostics to stderr, and the
# caller counts the failure in its own shell and skips the liveness check when AS_PID is empty.
# `tries` exists so this helper's own failure paths can be exercised quickly.
AS_PID=""
await_child() {
    local ready="$1" pidfile="$2" label="$3" tries="${4:-100}" i pid
    AS_PID=""
    for ((i = 0; i < tries; i++)); do
        if [[ -f "$ready" && -s "$pidfile" ]]; then break; fi
        sleep 0.1
    done
    if [[ ! -f "$ready" ]]; then
        echo "  FAIL $label: the child never signalled readiness" >&2
        return 1
    fi
    pid="$(cat "$pidfile" 2>/dev/null || true)"
    if [[ ! "$pid" =~ ^[0-9]+$ ]]; then
        echo "  FAIL $label: no usable child pid (got '${pid:-none}')" >&2
        return 1
    fi
    AS_PID="$pid"
    return 0
}

check() {
    local label="$1" expected="$2" needle="$3" file="$4" got
    got="$(cat "${exit_file:-$tmp/exit.txt}")"
    if [[ "$got" != "$expected" ]]; then
        echo "  FAIL $label: exit $got, wanted $expected"
        failures=$((failures + 1))
        return
    fi
    if [[ "$needle" != "-" ]] && ! grep -q -e "$needle" "$file"; then
        echo "  FAIL $label: '$needle' not found in $(basename "$file"); it said: $(head -c 200 "$file" | tr '\n' ' ')"
        failures=$((failures + 1))
        return
    fi
    echo "  ok   $label (exit $got)"
}

expect_value() {
    local label="$1" expected="$2" file="$3" got
    got="$(cat "$file" 2>/dev/null || true)"
    if [[ "$got" == "$expected" ]]; then
        echo "  ok   $label"
    else
        echo "  FAIL $label: got '$got', wanted '$expected'"
        failures=$((failures + 1))
    fi
}

expect() {
    local label="$1" needle="$2" file="$3"
    if [[ -f "$file" ]] && grep -q -e "$needle" "$file"; then
        echo "  ok   $label"
    else
        echo "  FAIL $label: '$needle' not found in $(basename "$file")"
        failures=$((failures + 1))
    fi
}

expect_absent() {
    local label="$1" needle="$2" file="$3"
    if grep -q -e "$needle" "$file"; then
        echo "  FAIL $label: '$needle' is present in $(basename "$file")"
        failures=$((failures + 1))
    else
        echo "  ok   $label"
    fi
}

echo "a successful round"
run ok
check "the review is produced" 0 - "$tmp/out.txt"
echo "  session: $(cat .agent-review/codex-session-id)  stdin: $(cat "$tmp/stdin.txt")"
check "the CLI cannot read stdin" 0 "rc=1" "$tmp/stdin.txt"

echo "the model the reviewer runs on"
expect_value "the first review asks for gpt-6.1-sol" "gpt-6.1-sol" "$tmp/last-model.txt"
# Read here, where that round really was the first one: later in the harness the same file holds the
# id of a resume, so asserting it there would be asserting the wrong round.
expect_value "and it really was a first review" "0" "$tmp/last-resume.txt"
expect_value "with Fast mode off by default" "default" "$tmp/last-tier.txt"
echo "a run that exited 0 without finishing its turn"
run no-completed
check "it is refused" 1 "turn.completed" "$tmp/err.txt"

echo "a completed turn with no final message"
run no-message
check "it is refused" 1 "no final message" "$tmp/err.txt"

echo "a completed turn whose message is not a verdict"
run not-verdict
check "it is refused" 1 "REVIEW-STATUS" "$tmp/err.txt"

echo "a message that merely mentions review"
run unclear
check "it is refused" 1 "REVIEW-STATUS" "$tmp/err.txt"

echo "a message only in the event stream, with no final-message file"
run events-only
check "it is refused" 1 "no final message" "$tmp/err.txt"
expect "the stream message is shown as evidence" "not the CLI's final message" "$tmp/err.txt"
expect "the stream message itself is shown" "审查结论" "$tmp/err.txt"

echo "messages that say the review has not finished"
counter=0
for text in "无法确认是否存在问题，需要继续审查。" \
            "我还没有检查并发问题，需要继续吗？" \
            "请先确认测试通过后，我再继续审查。"; do
    counter=$((counter + 1))
    STUB_TEXT="$text" run unfinished
    check "counter-example $counter is refused" 1 "REVIEW-STATUS" "$tmp/err.txt"
done

echo "status lines that are not the conclusion"
STUB_TEXT="$(printf '```\nREVIEW-STATUS: PASS\n```\n审查尚未完成，需要我继续吗？')" run unfinished
check "a status line inside a code block, with prose after it, is refused" 1 "REVIEW-STATUS" "$tmp/err.txt"
STUB_TEXT="$(printf 'REVIEW-STATUS: PASS 或 FINDINGS\n我还没有完成审查。')" run unfinished
check "a quoted format line is refused" 1 "REVIEW-STATUS" "$tmp/err.txt"
STUB_TEXT="$(printf '审查结论：本轮通过，未发现显著问题。\n\n**REVIEW-STATUS: PASS**')" run unfinished
check "a decorated but genuine status line is accepted" 0 - "$tmp/out.txt"

echo "git cannot say whether the tree changed"
# "Cannot check" must not read as "the precondition holds": the round is refused and the reviewer is never
# called. GIT_DIR pointing at nothing is a git failure that no repository state can mask.
calls_before_git_failure="$(wc -l < "$tmp/calls.txt" 2>/dev/null | tr -d ' ' || echo 0)"
set +e
GIT_DIR="$tmp/no-such-git-dir" STUB_MODE=ok PATH="$tmp/bin:$PATH" ./scripts/codex-review.sh >"$tmp/out.txt" 2>"$tmp/err.txt"
echo $? > "$tmp/exit.txt"
set -e
check "a git failure refuses the round" 66 "git failed" "$tmp/err.txt"
calls_after_git_failure="$(wc -l < "$tmp/calls.txt" 2>/dev/null | tr -d ' ' || echo 0)"
if [[ "$calls_before_git_failure" == "$calls_after_git_failure" ]]; then
    echo "  ok   the reviewer was not called"
else
    echo "  FAIL the reviewer was called $((calls_after_git_failure - calls_before_git_failure)) time(s) even though the tree could not be checked"
    failures=$((failures + 1))
fi

echo "a retry still needs a readable repository"
# Run in its own repository: corrupting the index of the harness's own repo would break every case after it.
# The root query succeeds there; the diff read is what fails. A retry is exempt from "there is nothing to
# review", never from "the repository cannot be read".
badrepo="$scratch/unreadable-repo"
mkdir -p "$badrepo/scripts" "$badrepo/bin"
cp "$helper" "$badrepo/scripts/"
cp -R "$tmp/bin/." "$badrepo/bin/"
cp -R .agent-review "$badrepo/" 2>/dev/null || true
(
    cd "$badrepo"
    git init -q .
    git config user.email b@c.i
    git config user.name b
    printf 'x\n' > f.txt
    git add -A >/dev/null 2>&1
    git commit -q -m base || true
    printf 'y\n' > f.txt
    # An interrupted round first, so the retry is a legitimate one and reaches the repository check. Its own
    # reviewer call is legitimate, so the baseline for "was the reviewer called" is taken after it.
    STUB_MODE=unfinished PATH="$badrepo/bin:$PATH" ./scripts/codex-review.sh >/dev/null 2>&1 || true
    wc -l < "$tmp/calls.txt" 2>/dev/null | tr -d ' ' > "$scratch/calls-before.txt" || echo 0 > "$scratch/calls-before.txt"
    printf 'not an index' > .git/index
    set +e
    STUB_MODE=ok PATH="$badrepo/bin:$PATH" ./scripts/codex-review.sh --retry >"$scratch/out.txt" 2>"$scratch/err.txt"
    echo $? > "$scratch/exit.txt"
    set -e
)
calls_before_unreadable="$(cat "$scratch/calls-before.txt" 2>/dev/null || echo 0)"
calls_after_unreadable="$(wc -l < "$tmp/calls.txt" 2>/dev/null | tr -d ' ' || echo 0)"
exit_file="$scratch/exit.txt"
check "a retry with an unreadable repository is refused" 66 "git failed" "$scratch/err.txt"
exit_file="$tmp/exit.txt"
if [[ "$calls_before_unreadable" == "$calls_after_unreadable" ]]; then
    echo "  ok   the reviewer was not called ($calls_after_unreadable call(s) so far)"
else
    echo "  FAIL the reviewer was called $((calls_after_unreadable - calls_before_unreadable)) time(s) on a repository it could not read"
    failures=$((failures + 1))
fi

echo "the call counter can tell that the reviewer ran"
# Without this, "the counter did not move" could also mean "the counter never moves".
calls_before_control="$(wc -l < "$tmp/calls.txt" 2>/dev/null | tr -d ' ' || echo 0)"
: > counter-control.txt
run ok
check "the control round runs" 0 - "$tmp/out.txt"
calls_after_control="$(wc -l < "$tmp/calls.txt" 2>/dev/null | tr -d ' ' || echo 0)"
if [[ "$calls_after_control" -gt "$calls_before_control" ]]; then
    echo "  ok   a real call moves the counter ($calls_before_control -> $calls_after_control)"
else
    echo "  FAIL a real call did not move the counter, so 'not called' proves nothing"
    failures=$((failures + 1))
fi

echo "the verdict must be the last line, whole"
# The helper used to skip a closing code fence, so a message that ended with a *format example* was read as a
# verdict -- the round this check exists to refuse.
STUB_TEXT="$(printf '审查尚未完成，以下只是格式示例：\n\n```\nREVIEW-STATUS: PASS\n```')" run unfinished
check "a fenced format example as the last line is refused" 1 "REVIEW-STATUS" "$tmp/err.txt"
STUB_TEXT="$(printf '本轮未完成。\n\nREVIEW-STATUS: pass')" run unfinished
check "a lowercase status token is refused" 1 "REVIEW-STATUS" "$tmp/err.txt"
STUB_TEXT="$(printf '本轮未完成。\n\nREVIEW-STATUS: PASS.')" run unfinished
check "a trailing period is refused" 1 "REVIEW-STATUS" "$tmp/err.txt"
STUB_TEXT="$(printf '本轮未完成。\n\n**REVIEW-STATUS: PASS')" run unfinished
check "an unpaired decoration is refused" 1 "REVIEW-STATUS" "$tmp/err.txt"
STUB_TEXT="$(printf '本轮通过。\n\n`REVIEW-STATUS: PASS`')" run unfinished
check "a backtick-paired status line is accepted" 0 - "$tmp/out.txt"

echo "a timeout written with a leading zero"
# `08` is not octal for a human, but bash arithmetic reads it as one: the comparison failed and the watchdog
# was never armed, so a hung run held the lock for ever.
run hang --timeout 08
check "the watchdog still ends it" 124 "did not finish within 8s" "$tmp/err.txt"

echo "an older round's streamed message is not this round's evidence"
# The stream fallback file was not cleared, so a failed round could print the previous round's message as its
# own evidence.
run events-only
check "the stream-only round is refused" 1 "no final message" "$tmp/err.txt"
if [[ -f .agent-review/last-review.jsonl.stream-message ]]; then
    run fail
    if grep -q "not the CLI's final message" "$tmp/err.txt"; then
        echo "  FAIL a stale streamed message was shown as this round's evidence"
        failures=$((failures + 1))
    else
        echo "  ok   the previous round's streamed message is not reused"
    fi
else
    echo "  FAIL no streamed message was written, so the reuse check could not run"
    failures=$((failures + 1))
fi

echo "a timeout too large for bash arithmetic"
# 0x10000000000000000 wraps to 0 in a 64-bit shell, which used to turn the watchdog off instead of refusing.
run hang --timeout 1000000000
check "an oversized timeout is refused" 64 "0 to 999999999" "$tmp/err.txt"

echo "a CLI that exits non-zero"
run fail
check "it is refused" 1 "exited 1" "$tmp/err.txt"
expect "the session is kept" "stub-session" .agent-review/codex-session-id

echo "a run that hangs"
run hang --timeout 2
check "the watchdog ends it" 124 "did not finish within 2s" "$tmp/err.txt"
expect "the session is kept" "stub-session" .agent-review/codex-session-id

echo "the --retry framing"
run ok --retry
check "the retry succeeds" 0 - "$tmp/out.txt"
expect "it says the run was interrupted" "You were interrupted" "$tmp/last-prompt.txt"
expect_value "the resumed review keeps the model" "gpt-6.1-sol" "$tmp/last-model.txt"
expect_value "and it really was a resume" "1" "$tmp/last-resume.txt"
expect_absent "it does not claim the code changed" "has updated the code" "$tmp/last-prompt.txt"

echo "a resume that fails"
run resume-fail
check "it is refused" 1 "exited 1" "$tmp/err.txt"
expect "it points at --fresh" "--fresh" "$tmp/err.txt"

echo "--fresh after a lost session"
run ok --fresh
check "the fresh review succeeds" 0 - "$tmp/out.txt"
expect "it warns that the memory is lost" "memory of earlier findings is gone" "$tmp/err.txt"

echo "the lock"
mkdir -p .agent-review/lock
run ok
check "a second review is refused" 75 "already running" "$tmp/err.txt"
expect "the refusal points at task keys" "--task <slug>" "$tmp/err.txt"
run ok --finish
check "--finish respects the lock" 75 "cannot be cleared" "$tmp/err.txt"
rmdir .agent-review/lock

echo "a task key keeps a session, a lock and an archive of its own"
run ok --task alpha
check "a named task reviews" 0 - "$tmp/out.txt"
expect_value "its session is its own" "stub-alpha" .agent-review/tasks/alpha/codex-session-id
expect_value "and the keyless session is untouched" "stub-session" .agent-review/codex-session-id
expect "the round says which task it is" "task: alpha (state: .agent-review/tasks/alpha" "$tmp/out.txt"
expect "the reviewer is told which session it is" "belongs to task 'alpha'" "$tmp/last-prompt.txt"
expect "and that the key does not filter what it reviews" "does not select, narrow or filter" "$tmp/last-prompt.txt"
if [[ -f .agent-review/tasks/alpha/round-counter ]]; then
    echo "  ok   the named task has a round counter of its own"
else
    echo "  FAIL the named task kept no counter"
    failures=$((failures + 1))
fi
if ls .agent-review/tasks/alpha/rounds/*-outcome.txt >/dev/null 2>&1; then
    echo "  ok   its round is archived under its own task"
else
    echo "  FAIL the named task's round was not archived"
    failures=$((failures + 1))
fi

echo "each task resumes only its own session"
run ok --task beta
check "a second task reviews" 0 - "$tmp/out.txt"
expect_value "it starts a session of its own" "0" "$tmp/last-resume.txt"
# A new session resumes nothing at all, which is also what gives the check below its teeth: a stub that
# recorded no id could not tell the two cases apart.
expect_value "and resumes nothing" "" "$tmp/last-resume-id.txt"
expect_value "named for that task" "stub-beta" .agent-review/tasks/beta/codex-session-id
run ok --task alpha
check "the first task reviews again" 0 - "$tmp/out.txt"
expect_value "as a resume, not a new session" "1" "$tmp/last-resume.txt"
expect_value "of its own session id, not another task's" "stub-alpha" "$tmp/last-resume-id.txt"
expect_value "of its own session" "stub-alpha" .agent-review/tasks/alpha/codex-session-id

echo "a task key is normalised and validated"
run ok --task Alpha
check "an uppercase key is accepted" 0 - "$tmp/out.txt"
# The resolved key is what is asserted, not the absence of a second directory: on a case-insensitive
# filesystem the two spellings are one directory whatever the script does, which is exactly why the
# normalisation is stated in the output instead of being left to the filesystem.
expect "the resolved key is printed" "task: alpha (state: .agent-review/tasks/alpha" "$tmp/out.txt"
expect "and the session it resumed is the lowercase one" "stub-alpha" "$tmp/out.txt"
for bad in "" "../escape" "Alpha Beta" "tasks" "rounds"; do
    run ok --task "$bad"
    check "the key '${bad:-<empty>}' is refused" 64 "Invalid task key" "$tmp/err.txt"
done
# The raw value is what is validated, so a newline a command substitution would have eaten cannot turn
# a malformed key into a valid one: $'alpha\n' must not become 'alpha' and act on its session.
run ok --task $'alpha\n'
check "a key with a trailing newline is refused" 64 "Invalid task key" "$tmp/err.txt"
expect_value "and the task it would have named is untouched" "stub-alpha" .agent-review/tasks/alpha/codex-session-id
OMT_REVIEW_TASK=$'gamma\n' run ok
check "the same key in the environment is refused" 64 "Invalid task key" "$tmp/err.txt"
if [[ -e .agent-review/tasks/gamma ]]; then
    echo "  FAIL the malformed env key wrote to a task directory"
    failures=$((failures + 1))
else
    echo "  ok   the malformed env key wrote nothing"
fi
long_key="$(printf 'a%.0s' {1..65})"
run ok --task "$long_key"
check "a key longer than 64 characters is refused" 64 "Invalid task key" "$tmp/err.txt"

if [[ -e .agent-review/escape || -e .agent-review/tasks/escape ]]; then
    echo "  FAIL a refused key still made a directory"
    failures=$((failures + 1))
else
    echo "  ok   a refused key creates nothing"
fi
# An empty key must not fall back to the environment either: "no task was named" and "the task named
# is empty" are different statements, and only one of them is true here.
OMT_REVIEW_TASK=gamma run ok --task ""
check "an empty key does not fall back to the env var" 64 "Invalid task key" "$tmp/err.txt"
if [[ -e .agent-review/tasks/gamma ]]; then
    echo "  FAIL the env var's task was used after all"
    failures=$((failures + 1))
else
    echo "  ok   nothing was written to the env var's task"
fi

echo "'default' is the keyless task's label, not a key to take"
run ok --task default
check "--task default is refused" 64 "Invalid task key" "$tmp/err.txt"
expect "the refusal says what that name is" "label of the task" "$tmp/err.txt"
run ok --task DEFAULT
check "in any case" 64 "Invalid task key" "$tmp/err.txt"
run ok --finish --task default
check "--finish cannot be aimed at the keyless task through it" 64 "Invalid task key" "$tmp/err.txt"
expect_value "so the keyless session is untouched" "stub-session" .agent-review/codex-session-id
OMT_REVIEW_TASK=default run ok
check "nor through the environment" 64 "Invalid task key" "$tmp/err.txt"
expect_value "with the keyless session still untouched" "stub-session" .agent-review/codex-session-id

echo "OMT_REVIEW_TASK names the task for a harness"
OMT_REVIEW_TASK=gamma run ok
check "the env var names the task" 0 - "$tmp/out.txt"
expect_value "its session is its own" "stub-gamma" .agent-review/tasks/gamma/codex-session-id
OMT_REVIEW_TASK=gamma run ok --task delta
check "a key on the command line wins" 0 - "$tmp/out.txt"
expect_value "over the env var" "stub-delta" .agent-review/tasks/delta/codex-session-id
expect_value "and leaves the env var's task alone" "stub-gamma" .agent-review/tasks/gamma/codex-session-id

echo "one task's lock is not another's"
mkdir -p .agent-review/tasks/alpha/lock
run ok
check "a task's lock does not block the keyless task" 0 - "$tmp/out.txt"
expect_absent "a keyless prompt names no task" "belongs to task" "$tmp/last-prompt.txt"
run ok --task beta
check "nor another task" 0 - "$tmp/out.txt"
run ok --task alpha
check "but it does block that task" 75 "already running" "$tmp/err.txt"
expect_absent "and the keyed refusal needs no hint" "--task <slug>" "$tmp/err.txt"
rmdir .agent-review/tasks/alpha/lock

echo "two tasks review at the same time"
STUB_MODE=hang PATH="$tmp/bin:$PATH" ./scripts/codex-review.sh --task alpha --timeout 60 >/dev/null 2>&1 &
alpha_pid=$!
for _ in $(seq 1 50); do [[ -d .agent-review/tasks/alpha/lock ]] && break; sleep 0.1; done
if [[ -d .agent-review/tasks/alpha/lock ]]; then
    echo "  ok   the hanging task holds its own lock"
else
    echo "  FAIL the hanging task never took its lock"
    failures=$((failures + 1))
fi
run ok
check "the keyless task reviews while it hangs" 0 - "$tmp/out.txt"
run ok --task beta
check "and so does another task" 0 - "$tmp/out.txt"
kill -TERM "$alpha_pid" 2>/dev/null
set +e
wait "$alpha_pid" 2>/dev/null
set -e
if [[ -d .agent-review/tasks/alpha/lock ]]; then
    echo "  FAIL the interrupted task left its lock behind"
    failures=$((failures + 1))
else
    echo "  ok   the interrupted task released its own lock"
fi

echo "--finish clears one task's session, not the others"
run ok --finish --task alpha
check "--finish takes the key" 0 - "$tmp/out.txt"
expect "and says which task it cleared" "task: alpha" "$tmp/out.txt"
if [[ -f .agent-review/tasks/alpha/codex-session-id ]]; then
    echo "  FAIL the named task's session survived --finish"
    failures=$((failures + 1))
else
    echo "  ok   the named task's session was cleared"
fi
expect_value "another task's session is untouched" "stub-beta" .agent-review/tasks/beta/codex-session-id
expect_value "and so is the keyless one" "stub-session" .agent-review/codex-session-id

echo "--tasks lists the sessions this repository holds"
run ok --tasks
check "the listing is produced" 0 - "$tmp/out.txt"
expect "the keyless task is listed" "^default" "$tmp/out.txt"
expect "with its session" "stub-session" "$tmp/out.txt"
expect "a named task is listed" "^beta" "$tmp/out.txt"
expect "with its own session" "stub-beta" "$tmp/out.txt"
expect_absent "a cleared task is not shown as holding one" "stub-alpha" "$tmp/out.txt"
run ok --tasks --design
check "a listing takes no round of its own" 64 "--tasks lists" "$tmp/err.txt"
run ok --tasks --task beta
check "and no task" 64 "--tasks lists" "$tmp/err.txt"

echo "the session log separates a clean hand-off from a lost session"
# The defect this guards: an outcome file writes `reason=none` both when a task ended on purpose and
# when its session went missing, so after the fact the two are indistinguishable. The log is the record
# that tells them apart, and these cases falsify it from both sides.
log=".agent-review/tasks/beta/session-log.tsv"
if [[ -f "$log" ]]; then
    echo "  ok   the log exists for a task that ran"
else
    echo "  FAIL no session log was written for the task"
    failures=$((failures + 1))
fi
expect "the first round is recorded as a session beginning" "$(printf '\tnew\tstub-beta\t')" "$log"
expect "and a later round as a resume" "$(printf '\tresumed\tstub-beta\t')" "$log"

# A session the CLI no longer holds must be recorded as lost, not as a resume -- this is the one case
# the whole log exists for, so it is asserted with the CLI's own error text.
echo "a resume against a session the CLI no longer holds"
run thread-gone --task beta
check "the round is refused" 1 "is gone" "$tmp/err.txt"
expect "the loss is named in the log" "$(printf '\tlost\tstub-beta\t')" "$log"
expect "and the log still shows the session it tried" "stub-beta" "$log"
# The counter-example: a lost session is not a closure. If `lost` were logged as a hand-off, a task
# whose session vanished would read as deliberately ended and its memory gap would go unnoticed.
expect_absent "a lost session is not recorded as finished" "$(printf '\tfinished\t')" "$log"

# --finish is the one deliberate end, and it must be distinguishable from the loss above.
echo "--finish records the hand-off it performs"
run ok --finish --task beta
check "--finish still takes the key" 0 - "$tmp/out.txt"
expect "the closure is recorded" "$(printf '\tfinished\tstub-beta\t')" "$log"
if [[ -f .agent-review/tasks/beta/codex-session-id ]]; then
    echo "  FAIL the pointer survived --finish"
    failures=$((failures + 1))
else
    echo "  ok   and only then does the pointer go"
fi

echo "--tasks reports whether a task is still on a session"
run ok --tasks
check "the listing is produced" 0 - "$tmp/out.txt"
expect "a closed task reads as closed" "closed" "$tmp/out.txt"
run ok --task gamma
check "a fresh task runs" 0 - "$tmp/out.txt"
run ok --tasks
expect "and reads as open, on its session" "open" "$tmp/out.txt"
run ok --tasks --task gamma
check "a listing still takes no task" 64 "--tasks lists" "$tmp/err.txt"

echo "a session replaced without --finish is not reported as a clean end"
# The second counter-example, and the harder one: a pointer that disappears without a --finish is a
# restart, not a hand-off. Deleting it here stands in for the ways that happens in practice (a failed
# first round that never recorded an id, a session cleared outside the helper).
gamma_log=".agent-review/tasks/gamma/session-log.tsv"
rm -f .agent-review/tasks/gamma/codex-session-id
run ok --task gamma
check "a round after the pointer vanished runs" 0 - "$tmp/out.txt"
expect "it is recorded as a restart" "$(printf '\trestarted\t')" "$gamma_log"
expect_absent "and not as a finished task" "$(printf '\tfinished\t')" "$gamma_log"

echo "a round after --finish is a new session, not an unexplained restart"
# The other half of the same distinction, and the defect the end-to-end probe caught: after a --finish
# the task *has* history, so a "has any history" test called the next round `restarted` and reported a
# deliberate hand-off as a session that went missing. It must be `new`, and it must not be `restarted`.
delta_log=".agent-review/tasks/delta/session-log.tsv"
run ok --task delta
check "the first round runs" 0 - "$tmp/out.txt"
run ok --finish --task delta
check "--finish takes the key" 0 - "$tmp/out.txt"
run ok --task delta
check "the round after --finish runs" 0 - "$tmp/out.txt"
expect "it is recorded as a new session" "$(printf '\tnew\tstub-delta\t')" "$delta_log"
expect_absent "not as a restart" "$(printf '\trestarted\t')" "$delta_log"
if [[ "$(grep -c "$(printf '\tnew\t')" "$delta_log")" == "2" ]]; then
    echo "  ok   both sessions began with a new event"
else
    echo "  FAIL expected two session beginnings: $(cat "$delta_log")"
    failures=$((failures + 1))
fi

echo "a failed attempt that never created a session is not history"
# The third defect from the review: a first round whose CLI dies before thread.started writes an
# outcome with `session=none`. Treating the archive as proof that a session existed made the *next*
# round `restarted`, reporting a replacement of a session that never was. It must be `new`, and the
# no-session attempt must not appear in the log at all -- there is no session to name.
zeta_log=".agent-review/tasks/zeta/session-log.tsv"
run no-session --task zeta
check "an attempt with no session is still archived" 0 - "$tmp/out.txt"
if [[ -f "$zeta_log" ]]; then
    echo "  FAIL a session event was logged for an attempt that had no session: $(cat "$zeta_log")"
    failures=$((failures + 1))
else
    echo "  ok   an attempt that established no session logs no session event"
fi
expect_absent "and the health view does not claim a session" "stub-zeta" "$tmp/out.txt"
run ok --task zeta
check "the next round runs" 0 - "$tmp/out.txt"
expect "it begins a session rather than replacing one" "$(printf '\tnew\tstub-zeta\t')" "$zeta_log"
expect_absent "and is not called a restart" "$(printf '\trestarted\t')" "$zeta_log"

echo "the round number is the same in the log and the archive"
# The second defect from the review: the log read the counter before the round and the archive wrote it
# after, so every entry was one round off and an event could not be matched to the evidence it
# described. The number in the log must name an archived outcome that exists.
num_task="numbers"
num_log=".agent-review/tasks/$num_task/session-log.tsv"
for i in 1 2 3; do
    run ok --task "$num_task"
    check "round $i runs" 0 - "$tmp/out.txt"
done
for n in 1 2 3; do
    if [[ "$(awk -F'\t' -v n="$n" '$4 == n { c++ } END { print c + 0 }' "$num_log")" == "1" ]]; then
        echo "  ok   the log names round $n exactly once"
    else
        echo "  FAIL the log does not name round $n exactly once: $(cat "$num_log")"
        failures=$((failures + 1))
    fi
    if ls ".agent-review/tasks/$num_task/rounds/"*-"$(printf '%03d' "$n")"-outcome.txt >/dev/null 2>&1; then
        echo "  ok   and an archive outcome carries $n"
    else
        echo "  FAIL no archive outcome carries round $n: $(ls ".agent-review/tasks/$num_task/rounds/" | tr '\n' ' ')"
        failures=$((failures + 1))
    fi
done
# The third round of the session is the third the session has run, and the budget warning counts
# attempts, not events: a loss is the second half of a resume and must not be counted twice.
run ok --tasks
expect "the session counts its own rounds" "3" "$tmp/out.txt"

echo "a lost session is counted once, not once per event"
# The fourth defect from the review: `lost` was counted as an extra round on top of the `resumed` it
# belongs to, so the session looked busier than it was and the budget warning fired early.
loss_task="losses"
loss_log=".agent-review/tasks/$loss_task/session-log.tsv"
run ok --task "$loss_task"
check "the first round runs" 0 - "$tmp/out.txt"
run thread-gone --task "$loss_task"
check "the resume fails as a lost session" 1 "is gone" "$tmp/err.txt"
run ok --task "$loss_task"
check "a fresh round runs again" 0 - "$tmp/out.txt"
if [[ "$(grep -c "$(printf '\tlost\t')" "$loss_log")" == "1" ]]; then
    echo "  ok   the loss is recorded"
else
    echo "  FAIL the loss was not recorded once: $(cat "$loss_log")"
    failures=$((failures + 1))
fi
# Two attempts ran (the first round and the retry), so the session has spent two rounds, not three --
# the lost resume and the round after it are one attempt each, and the loss is not a third.
resumes="$(grep -c "$(printf '\tresumed\t')" "$loss_log")"
if [[ "$resumes" == "2" ]]; then
    echo "  ok   the two resumed attempts are on record"
else
    echo "  FAIL expected two resumes, got $resumes: $(cat "$loss_log")"
    failures=$((failures + 1))
fi
# The counting rule itself: new(1) + resumed(2) + lost(not counted) + resumed(3) is three rounds spent
# across two sessions' worth of log lines -- the loss must not make it four.
run ok --tasks
if awk '$1 == "losses" { print $7 }' "$tmp/out.txt" | grep -qx "3"; then
    echo "  ok   the loss is not counted as an extra round"
else
    echo "  FAIL the session's round count is wrong: $(awk '$1 == "losses"' "$tmp/out.txt")"
    failures=$((failures + 1))
fi

echo "an interrupted first round still records that its session began"
# The defect this guards: the log entry for a new session is written after the CLI returns, but a round
# stopped by a signal exits through the handler before reaching that line. Without a record on both
# paths the log would open on a later round's `resumed`, leaving the session's beginning unrecorded.
# A real SIGTERM is used, not --timeout: the timeout returns through the normal path and would pass
# even with the signal path broken, which is exactly how this assertion first failed to bite.
int_task="interrupted-start"
rm -rf ".agent-review/tasks/$int_task"
STUB_MODE=hang PATH="$tmp/bin:$PATH" ./scripts/codex-review.sh --task "$int_task" >/dev/null 2>&1 &
int_pid=$!
for _ in $(seq 1 100); do [[ -s ".agent-review/tasks/$int_task/last-review.jsonl" ]] && break; sleep 0.1; done
for _ in $(seq 1 100); do [[ -s ".agent-review/tasks/$int_task/codex-session-id" ]] && break; sleep 0.1; done
kill -TERM "$int_pid" 2>/dev/null
set +e
wait "$int_pid"
set -e
int_log=".agent-review/tasks/$int_task/session-log.tsv"
expect "the interrupted first round records its session" "$(printf '\tnew\tstub-interrupted-start\t')" "$int_log"
expect_value "and the pointer names the same session it logged" "stub-interrupted-start" \
    ".agent-review/tasks/$int_task/codex-session-id"
# The idempotence half: one round must contribute exactly one event, not one per path.
if [[ "$(grep -c "$(printf '\tnew\t')" "$int_log")" == "1" ]]; then
    echo "  ok   the round is recorded once, not once per path"
else
    echo "  FAIL the round was recorded more than once: $(cat "$int_log")"
    failures=$((failures + 1))
fi

echo "the session-log write is not gated by the in-memory flag"
# The window the review found: with ROUND_SESSION_LOGGED set before the append, a signal landing
# between them loses the entry -- verified by widening that window and delivering SIGTERM inside it,
# which leaves the round archived with no session event at all. The fix removes the dependence on that
# ordering (the log itself is the idempotence key), so the ordering is what is asserted here.
#
# This is a static check on purpose, and the reason is worth stating: the window is two adjacent shell
# statements, so no test can deterministically land a signal inside it -- a timing-based test would be
# flaky, and a flaky gate is worse than none. Reading the ordering off the source fails on the
# defective implementation every time, which is what a regression gate has to do.
log_body="$(sed -n '/^log_round_session()/,/^}/p' ./scripts/codex-review.sh)"
append_line="$(printf '%s\n' "$log_body" | grep -n 'log_session_event' | head -1 | cut -d: -f1)"
guard_line="$(printf '%s\n' "$log_body" | grep -n 'ROUND_SESSION_LOGGED=1' | tail -1 | cut -d: -f1)"
if [[ -n "$append_line" && -n "$guard_line" && "$guard_line" -gt "$append_line" ]]; then
    echo "  ok   the flag is set after the append, never before it"
else
    echo "  FAIL the flag is set at or before the append (flag=$guard_line append=$append_line): a signal"
    echo "       between them would lose the event, which is the defect this fix removed"
    failures=$((failures + 1))
fi
# The other direction, so the check cannot pass on an implementation that never sets it at all.
if printf '%s\n' "$log_body" | grep -q 'log_session_event'; then
    echo "  ok   and the append is what the ordering is about"
else
    echo "  FAIL log_round_session no longer appends an event"
    failures=$((failures + 1))
fi

echo "the round budget warning"
# AGENTS.md budgets three review rounds per task, so the session is over budget when it starts a
# fourth. The warning must stay silent through the first three and fire on the fourth: one that always
# fires is noise, and one that never fires is not a gate. Both sides are asserted here.
budget_task="budget"
for i in 1 2 3 4; do
    run ok --task "$budget_task"
    check "budget round $i runs" 0 - "$tmp/out.txt"
    if [[ "$i" -le 3 ]]; then
        expect_absent "round $i does not warn yet" "budgets 3" "$tmp/err.txt"
    else
        expect "round $i warns that the session is over budget" "budgets 3" "$tmp/err.txt"
        expect "and names the way out" "--finish" "$tmp/err.txt"
    fi
done

echo "a signal sent to the script alone, while the CLI ignores TERM"
rm -f "$tmp/child.pid" "$tmp/script-exit.txt"
STUB_MODE=ignore-term PATH="$tmp/bin:$PATH" ./scripts/codex-review.sh >/dev/null 2>&1 &
script_pid=$!
for _ in $(seq 1 50); do [[ -s "$tmp/child.pid" ]] && break; sleep 0.1; done
child_pid="$(cat "$tmp/child.pid" 2>/dev/null || echo none)"
before=$(ls .agent-review/rounds/*-outcome.txt 2>/dev/null | wc -l | tr -d ' ')
kill -TERM "$script_pid" 2>/dev/null
set +e
wait "$script_pid"
echo $? > "$tmp/script-exit.txt"
set -e
echo "  exit $(cat "$tmp/script-exit.txt") (want 143), CLI pid was $child_pid"
if [[ "$(cat "$tmp/script-exit.txt")" == "143" ]]; then echo "  ok   the script reports the interruption"; else echo "  FAIL exit $(cat "$tmp/script-exit.txt")"; failures=$((failures+1)); fi
if kill -0 "$child_pid" 2>/dev/null; then echo "  FAIL the CLI is still alive"; failures=$((failures+1)); else echo "  ok   the CLI was stopped too"; fi
if [[ -d .agent-review/lock ]]; then echo "  FAIL the lock is still held"; failures=$((failures+1)); else echo "  ok   the lock was released"; fi
after=$(ls .agent-review/rounds/*-outcome.txt 2>/dev/null | wc -l | tr -d ' ')
if [[ "$after" -gt "$before" ]]; then echo "  ok   the interrupted round was archived"; else echo "  FAIL nothing archived"; failures=$((failures+1)); fi

echo "a group whose leader yields but whose child ignores TERM"
rm -f "$tmp/child.pid" "$tmp/grandchild.pid" "$tmp/child-ready"
STUB_MODE=child-survives PATH="$tmp/bin:$PATH" ./scripts/codex-review.sh >/dev/null 2>&1 &
script_pid=$!
grandchild=""
await_child "$tmp/child-ready" "$tmp/grandchild.pid" "the signal case handshake" || failures=$((failures + 1))
grandchild="$AS_PID"
kill -TERM "$script_pid" 2>/dev/null
set +e
wait "$script_pid"
echo $? > "$tmp/exit.txt"
set -e
check "the interruption is reported" 143 - "$tmp/exit.txt"
if [[ -z "$grandchild" ]]; then
    echo "  ok   (no usable pid, so the liveness check is skipped; the handshake failure above counts)"
elif kill -0 "$grandchild" 2>/dev/null; then
    echo "  FAIL the child that ignored TERM is still alive"
    failures=$((failures + 1))
else
    echo "  ok   the child that ignored TERM was killed with the group"
fi

echo "the same, on the timeout path"
rm -f "$tmp/child.pid" "$tmp/grandchild.pid" "$tmp/child-ready"
STUB_MODE=child-survives PATH="$tmp/bin:$PATH" ./scripts/codex-review.sh --timeout 2 >/dev/null 2>"$tmp/err.txt" &
script_pid=$!
grandchild=""
await_child "$tmp/child-ready" "$tmp/grandchild.pid" "the timeout case handshake" || failures=$((failures + 1))
grandchild="$AS_PID"
set +e
wait "$script_pid"
echo $? > "$tmp/exit.txt"
set -e
check "the timeout is reported" 124 "did not finish within 2s" "$tmp/err.txt"
if [[ -z "$grandchild" ]]; then
    echo "  ok   (no usable pid, so the liveness check is skipped; the handshake failure above counts)"
elif kill -0 "$grandchild" 2>/dev/null; then
    echo "  FAIL the child that ignored TERM survived the timeout"
    failures=$((failures + 1))
else
    echo "  ok   the timeout killed the whole group"
fi

echo "a second signal during the cleanup"
rm -f "$tmp/child.pid" "$tmp/grandchild.pid" "$tmp/child-ready"
# Job control for the launch, or the backgrounded script inherits SIGINT as ignored (a non-interactive
# shell's asynchronous children do) and the second signal below would be a no-op — which is exactly how
# an earlier version of this case passed without testing anything.
set -m
STUB_MODE=child-survives PATH="$tmp/bin:$PATH" ./scripts/codex-review.sh >/dev/null 2>&1 &
script_pid=$!
set +m
grandchild=""
await_child "$tmp/child-ready" "$tmp/grandchild.pid" "the cross-signal handshake" || failures=$((failures + 1))
grandchild="$AS_PID"
before=$(ls .agent-review/rounds/*-outcome.txt 2>/dev/null | wc -l | tr -d ' ')
kill -TERM "$script_pid" 2>/dev/null
sleep 0.3
kill -INT "$script_pid" 2>/dev/null
set +e
wait "$script_pid"
echo $? > "$tmp/exit.txt"
set -e
check "the first signal decides the exit" 143 - "$tmp/exit.txt"
if [[ -z "$grandchild" ]]; then
    echo "  ok   (no usable pid, so the liveness check is skipped; the handshake failure above counts)"
elif kill -0 "$grandchild" 2>/dev/null; then
    echo "  FAIL the CLI outlived the cleanup"
    failures=$((failures + 1))
else
    echo "  ok   the CLI was still stopped"
fi
if [[ -d .agent-review/lock ]]; then
    echo "  FAIL the lock was left behind"
    failures=$((failures + 1))
else
    echo "  ok   the lock was released"
fi
after=$(ls .agent-review/rounds/*-outcome.txt 2>/dev/null | wc -l | tr -d ' ')
if [[ "$after" -gt "$before" ]]; then
    echo "  ok   the round was archived despite the second signal"
else
    echo "  FAIL the second signal skipped the archive"
    failures=$((failures + 1))
fi

echo "the model and speed the reviewer runs on, on every round"
# The default case is the file absent, which is the state of this harness: the built-in default model
# with Fast mode off. Both halves are asserted, because "off" is a value the helper sends
# (service_tier=default) rather than the absence of an argument.
expect_value "the reviewer runs on gpt-6.1-sol" "gpt-6.1-sol" "$tmp/last-model.txt"
expect_value "with Fast mode off" "default" "$tmp/last-tier.txt"
expect "the round says which settings it used" "reviewer: model=gpt-6.1-sol (the built-in default), fast=off (the built-in default)" "$tmp/out.txt"

# A resume has to repeat the tier: codex resolves it per turn, so a resumed round that omits it runs at
# the default speed even when the first round asked for Fast (measured on codex-cli 0.162.1).
run ok
expect_value "a re-review is a resume" "1" "$tmp/last-resume.txt"
expect_value "and it carries the tier again" "default" "$tmp/last-tier.txt"

echo "OMT_REVIEW_MODEL names another model, OMT_REVIEW_FAST another speed"
OMT_REVIEW_MODEL=another-model run ok
check "the run with another model is produced" 0 - "$tmp/out.txt"
expect_value "OMT_REVIEW_MODEL is passed through" "another-model" "$tmp/last-model.txt"
expect "the round says where the model came from" "model=another-model (OMT_REVIEW_MODEL)" "$tmp/out.txt"
expect_value "and the speed is untouched" "default" "$tmp/last-tier.txt"
OMT_REVIEW_FAST=1 run ok
check "the run with Fast mode is produced" 0 - "$tmp/out.txt"
expect_value "OMT_REVIEW_FAST=1 asks for codex's priority tier" "priority" "$tmp/last-tier.txt"
expect "the round says where the speed came from" "fast=on (OMT_REVIEW_FAST)" "$tmp/out.txt"
OMT_REVIEW_FAST=false run ok
expect_value "OMT_REVIEW_FAST=false is off" "default" "$tmp/last-tier.txt"
for bad in maybe 2 yes-please; do
    OMT_REVIEW_FAST="$bad" run ok
    check "OMT_REVIEW_FAST='$bad' is refused" 64 "OMT_REVIEW_FAST" "$tmp/err.txt"
done
OMT_REVIEW_MODEL=$'bad\nmodel' run ok
check "a model with a newline is refused" 64 "OMT_REVIEW_MODEL" "$tmp/err.txt"
OMT_REVIEW_MODEL='a b' run ok
check "a model with a space is refused" 64 "OMT_REVIEW_MODEL" "$tmp/err.txt"

echo "codex-review.toml configures the model and the speed"
# The file is read from the repository root, which is this throwaway repo. Each case rewrites it; the
# last one removes it so the remaining cases run on the built-in defaults.
write_config() { printf '%s\n' "$@" > codex-review.toml; }
# A fast flag whose value cannot be confused with the file's: every case below that asserts a "default"
# tier would also pass if the flag were dropped.
write_config '[review]' 'model = "file-model"' 'fast = true'
run ok
check "a configured round runs" 0 - "$tmp/out.txt"
expect_value "the file's model is used" "file-model" "$tmp/last-model.txt"
expect_value "the file's fast = true asks for priority" "priority" "$tmp/last-tier.txt"
expect "the round names the file as the source" "model=file-model (codex-review.toml), fast=on (codex-review.toml)" "$tmp/out.txt"
write_config '[review]' 'fast = true'
run ok
expect_value "a missing model falls back on its own" "gpt-6.1-sol" "$tmp/last-model.txt"
expect_value "while the file's fast still applies" "priority" "$tmp/last-tier.txt"
expect "each setting's source is reported separately" "model=gpt-6.1-sol (the built-in default), fast=on (codex-review.toml)" "$tmp/out.txt"
write_config '[review]' 'model = "file-model"' 'fast = false'
run ok
expect_value "fast = false is off" "default" "$tmp/last-tier.txt"
# The file must not be able to leave the tier unset: an omitted service_tier inherits a Fast mode
# configured in ~/.codex/config.toml or .codex/config.toml, so "off" has to be sent as "default".
OMT_REVIEW_FAST=1 run ok
expect_value "a one-off override wins over the file" "priority" "$tmp/last-tier.txt"
OMT_REVIEW_MODEL=env-model run ok
expect_value "and the model override wins over the file" "env-model" "$tmp/last-model.txt"

echo "a configuration that cannot be read refuses the round"
calls_before_config="$(wc -l < "$tmp/calls.txt" 2>/dev/null | tr -d ' ' || echo 0)"
for bad_config in 'model = [' 'fast = "yes"' 'model = 12' 'model = ""' '[review]
unknown = 1' '[other]
model = "x"' '[review]
model = "has
newline"' 'review = 1' '[review]
fast = 1'; do
    printf '%s\n' "$bad_config" > codex-review.toml
    run ok
    check "it is refused: $(printf '%s' "$bad_config" | tr '\n' ' ' | head -c 40)" 64 "codex-review.toml" "$tmp/err.txt"
done
# The missing TOML reader has to be refused in the same way: a bare `python3` is not necessarily 3.11+.
printf '%s\n' '[review]' 'model = "file-model"' > codex-review.toml
rm -rf "$tmp/nolib"; mkdir -p "$tmp/nolib"
cat > "$tmp/nolib/python3" <<'NOPY'
#!/bin/bash
# A python3 without tomllib (macOS ships 3.9 at /usr/bin/python3), for the version check.
if [[ "${1:-}" == "-" ]]; then
    echo "python3 3.9.6 has no tomllib, so $2 cannot be read; Python 3.11 or newer is needed for that file. Runs without it need no TOML reader." >&2
    exit 1
fi
exec /usr/bin/env python3 "$@"
NOPY
chmod +x "$tmp/nolib/python3"
set +e
STUB_MODE=ok PATH="$tmp/nolib:$tmp/bin:$PATH" ./scripts/codex-review.sh >"$tmp/out.txt" 2>"$tmp/err.txt"
echo $? > "$tmp/exit.txt"
set -e
check "a python3 without tomllib is refused, with the reason" 64 "no tomllib" "$tmp/err.txt"
# The counter-example that makes "the reviewer was not called" mean something: an accepted config does
# call it. Without this, a harness that never calls the stub at all would pass the check below.
write_config '[review]' 'model = "file-model"'
run ok
check "a readable configuration still runs" 0 - "$tmp/out.txt"
calls_after_control="$(wc -l < "$tmp/calls.txt" 2>/dev/null | tr -d ' ' || echo 0)"
if [[ "$calls_after_control" -gt "$calls_before_config" ]]; then
    echo "  ok   an accepted configuration does call the reviewer"
else
    echo "  FAIL the reviewer was never called, so 'not called' proves nothing"
    failures=$((failures + 1))
fi
# Every refused case above must have left the CLI alone.
calls_before_bad="$(wc -l < "$tmp/calls.txt" 2>/dev/null | tr -d ' ' || echo 0)"
printf '%s\n' '[review]' 'fast = "yes"' > codex-review.toml
run ok
check "a bad value is refused" 64 "must be true or false" "$tmp/err.txt"
run ok --task alpha
check "a bad value refuses a named task too" 64 "must be true or false" "$tmp/err.txt"
calls_after_bad="$(wc -l < "$tmp/calls.txt" 2>/dev/null | tr -d ' ' || echo 0)"
if [[ "$calls_before_bad" == "$calls_after_bad" ]]; then
    echo "  ok   a refused configuration never reaches the reviewer"
else
    echo "  FAIL the reviewer was called $((calls_after_bad - calls_before_bad)) time(s) with a broken configuration"
    failures=$((failures + 1))
fi
# A refused configuration must leave the previous round's evidence where it was.
: > .agent-review/last-status.txt; printf 'concluded=yes\n' > .agent-review/last-status.txt
: > .agent-review/last-round-kind; printf 'review\n' > .agent-review/last-round-kind
run ok
check "the round is refused" 64 "codex-review.toml" "$tmp/err.txt"
expect "the previous status record survives" "concluded=yes" .agent-review/last-status.txt
expect_value "and the recorded kind is untouched" "review" .agent-review/last-round-kind
# A broken configuration must not be able to make a session unclearable.
write_config '[review]' 'model = ['
run ok --tasks
check "--tasks works with a broken configuration" 0 "task" "$tmp/out.txt"
run ok --finish
check "--finish works with a broken configuration" 0 "session cleared" "$tmp/out.txt"
run ok --help
check "--help works with a broken configuration" 0 "usage:" "$tmp/err.txt"
rm -f codex-review.toml

echo "a configuration path that exists but cannot be read is refused, not treated as absent"
# "No configuration file" and "a configuration that cannot be read" are different states, and only the
# first may fall back on the defaults. Treating the second as absent is the silent fallback this
# function exists to prevent: a config reached through a symlink would stop applying, without a word,
# the moment its target went away, and the reviewer would run on a model nobody chose.
config_calls_before="$(wc -l < "$tmp/calls.txt" 2>/dev/null | tr -d ' ' || echo 0)"
rm -f codex-review.toml
mkdir codex-review.toml
run ok
check "a directory where the configuration should be is refused" 64 "not a readable regular file" "$tmp/err.txt"
rmdir codex-review.toml
ln -s "$tmp/no-such-target.toml" codex-review.toml
run ok
check "a dangling symlink is refused" 64 "not a readable regular file" "$tmp/err.txt"
rm -f codex-review.toml
# The absent case is asserted straight after, so "refused" cannot be passing because every state is
# refused: with the path gone the built-in defaults are used and the reviewer does run.
run ok
check "a genuinely absent configuration still uses the defaults" 0 - "$tmp/out.txt"
expect_value "on the default model" "gpt-6.1-sol" "$tmp/last-model.txt"
# Unreadable to this user, which is not the same as absent either.
printf '%s\n' '[review]' 'model = "file-model"' > codex-review.toml
chmod 000 codex-review.toml
if [[ ! -r codex-review.toml ]]; then
    run ok
    check "an unreadable file is refused" 64 "not a readable regular file" "$tmp/err.txt"
else
    echo "  ok   (skipped the chmod-000 case: this user can still read the file)"
fi
chmod 644 codex-review.toml 2>/dev/null || true
rm -f codex-review.toml
config_calls_after="$(wc -l < "$tmp/calls.txt" 2>/dev/null | tr -d ' ' || echo 0)"
# The single call in the absent-configuration control above is the only one that may have happened here.
if [[ "$((config_calls_after - config_calls_before))" -le 1 ]]; then
    echo "  ok   only the absent-configuration control reached the reviewer"
else
    echo "  FAIL $((config_calls_after - config_calls_before)) calls happened; a broken path reached the reviewer"
    failures=$((failures + 1))
fi

echo "the requested speed is archived with every round"
run ok
last_outcome="$(ls -t .agent-review/rounds/*-outcome.txt | head -1)"
expect "the outcome names the model" "model=gpt-6.1-sol" "$last_outcome"
expect "and its source" "model_source=the built-in default" "$last_outcome"
expect "and the speed" "fast=off" "$last_outcome"
expect "and the tier actually requested" "service_tier=default" "$last_outcome"
expect "and that nothing was rejected" "service_tier_rejected=no" "$last_outcome"

echo "a speed codex declines is reported and archived, not treated as a failed round"
STUB_MODE=tier-rejected run tier-rejected
check "the review still counts" 0 - "$tmp/out.txt"
# A specific field inside a multi-field file, so grep rather than expect_value (which reads the whole file).
if grep -q '^service_tier_rejected=yes$' .agent-review/last-status.txt; then
    echo "  ok   the status records the rejected tier"
else
    echo "  FAIL the status did not record the rejected tier"
    failures=$((failures + 1))
fi
last_outcome="$(ls -t .agent-review/rounds/*-outcome.txt | head -1)"
expect "the outcome carries the rejection" "service_tier_rejected=yes" "$last_outcome"
expect "and the round still produced its review" "reason=none" "$last_outcome"
STUB_MODE=error-item run error-item
check "an unrelated error item does not fail the round" 0 - "$tmp/out.txt"
# The fixture is asserted to have emitted its error: without this, a call that silently ran the happy
# path would satisfy every "no tier rejection" assertion below it.
expect "the fixture really emitted an error item" "stream disconnected before completion" .agent-review/last-review.jsonl
expect_absent "and it is not reported as a tier rejection" "did not accept the requested service tier" "$tmp/err.txt"
last_outcome="$(ls -t .agent-review/rounds/*-outcome.txt | head -1)"
expect "with no rejection in the archive" "service_tier_rejected=no" "$last_outcome"
# The same generic phrase about a different setting: matching it alone would archive a tier rejection
# that never happened, so this is the case that gives the tier-specific match its teeth. `run` takes the
# mode as its *first* argument, so the mode is passed there and not only as a prefix: a prefix naming a
# mode that the call then overrides runs the happy path, and the counter-example proves nothing.
STUB_MODE=other-setting run other-setting
check "a rejection of another setting is not a failed round" 0 - "$tmp/out.txt"
expect "the fixture really emitted the other setting's rejection" "reasoning effort" .agent-review/last-review.jsonl
expect "and the generic phrase really was in it" "not advertised as supported for model" .agent-review/last-review.jsonl
expect_absent "and it is not reported as a tier rejection either" "did not accept the requested service tier" "$tmp/err.txt"
last_outcome="$(ls -t .agent-review/rounds/*-outcome.txt | head -1)"
expect "with no tier rejection in the archive either" "service_tier_rejected=no" "$last_outcome"

echo "the handshake helper's own failure paths"
rm -f "$tmp/nope-ready" "$tmp/bad.pid"
: > "$tmp/bad.pid"; echo "not-a-pid" > "$tmp/bad.pid"
if await_child "$tmp/nope-ready" "$tmp/bad.pid" "missing readiness" 2 2>/dev/null; then
    echo "  FAIL a missing readiness file was accepted"
    failures=$((failures + 1))
else
    echo "  ok   a missing readiness file is refused"
fi
: > "$tmp/ok-ready"
if await_child "$tmp/ok-ready" "$tmp/bad.pid" "non-numeric pid" 2 2>/dev/null; then
    echo "  FAIL a non-numeric pid was accepted"
    failures=$((failures + 1))
else
    echo "  ok   a non-numeric pid is refused"
fi
if await_child "$tmp/ok-ready" "$tmp/grandchild.pid" "a real pid" 2 2>/dev/null; then
    echo "  ok   a real pid is accepted"
else
    echo "  FAIL a real pid was refused"
    failures=$((failures + 1))
fi

echo "a session created while the CLI was being stopped"
rm -f "$tmp/child.pid" "$tmp/late-ready" "$tmp/late-go" .agent-review/codex-session-id
STUB_MODE=late-session PATH="$tmp/bin:$PATH" ./scripts/codex-review.sh >/dev/null 2>&1 &
script_pid=$!
for _ in $(seq 1 100); do [[ -f "$tmp/late-ready" ]] && break; sleep 0.1; done
kill -TERM "$script_pid" 2>/dev/null
: > "$tmp/late-go"
set +e
wait "$script_pid"
echo $? > "$tmp/exit.txt"
set -e
check "the interruption is reported" 143 - "$tmp/exit.txt"
if [[ "$(cat .agent-review/codex-session-id 2>/dev/null)" == "late-session" ]]; then
    echo "  ok   the session created during the grace period was kept"
else
    echo "  FAIL the late session was lost: $(cat .agent-review/codex-session-id 2>/dev/null || echo none)"
    failures=$((failures + 1))
fi

echo "a timeout against a CLI that ignores TERM"
rm -f "$tmp/child.pid"
STUB_MODE=ignore-term PATH="$tmp/bin:$PATH" ./scripts/codex-review.sh --timeout 2 >/dev/null 2>"$tmp/err.txt" &
script_pid=$!
for _ in $(seq 1 50); do [[ -s "$tmp/child.pid" ]] && break; sleep 0.1; done
child_pid="$(cat "$tmp/child.pid" 2>/dev/null || echo none)"
started=$SECONDS
set +e
wait "$script_pid"
echo $? > "$tmp/exit.txt"
set -e
elapsed=$((SECONDS - started))
check "the watchdog ends it" 124 "did not finish within 2s" "$tmp/err.txt"
echo "  it took ${elapsed}s after the timeout to come back (want < 15)"
if [[ "$elapsed" -lt 15 ]]; then echo "  ok   the termination is bounded"; else echo "  FAIL it held on for ${elapsed}s"; failures=$((failures+1)); fi
if kill -0 "$child_pid" 2>/dev/null; then echo "  FAIL the CLI survived KILL"; failures=$((failures+1)); else echo "  ok   the CLI was killed"; fi

echo "a captured stdout must reach EOF when the script returns"
started=$SECONDS
set +e
STUB_MODE=ok PATH="$tmp/bin:$PATH" ./scripts/codex-review.sh --timeout 60 2>/dev/null | cat >/dev/null
set -e
elapsed=$((SECONDS - started))
echo "  the pipeline closed after ${elapsed}s (want < 20, not the 60s timeout)"
if [[ "$elapsed" -lt 20 ]]; then echo "  ok   no process keeps the pipe open"; else echo "  FAIL the pipe was held for ${elapsed}s"; failures=$((failures+1)); fi

echo "the archive"
echo "  outcomes: $(ls .agent-review/rounds/*-outcome.txt | wc -l | tr -d ' ')  events: $(ls .agent-review/rounds/*-events.jsonl | wc -l | tr -d ' ')  reviews: $(ls .agent-review/rounds/*-review.txt | wc -l | tr -d ' ')  stderr: $(ls .agent-review/rounds/*-stderr.txt | wc -l | tr -d ' ')"
if grep -q -e "turn.completed" .agent-review/rounds/*-outcome.txt; then
    echo "  ok   the failed round's reason is archived"
else
    echo "  FAIL the failed round's reason is not archived"
    failures=$((failures + 1))
fi
if grep -q -e "boom: transport reset" .agent-review/rounds/*-stderr.txt; then
    echo "  ok   the transport error text is archived, not left to be overwritten"
else
    echo "  FAIL the CLI's stderr was not archived"
    failures=$((failures + 1))
fi
if grep -q -e "interrupted (signal" .agent-review/rounds/*-outcome.txt; then
    echo "  ok   the interrupted round says so"
else
    echo "  FAIL the interruption was not recorded"
    failures=$((failures + 1))
fi

echo "a relative --note-file"
echo "备注内容：只审脚本。" > scripts/note.txt
set +e
(cd scripts && STUB_MODE=ok PATH="$tmp/bin:$PATH" ./codex-review.sh --note-file ./note.txt >/dev/null 2>&1)
echo $? > "$tmp/exit.txt"
set -e
check "it is read from the caller's directory" 0 - "$tmp/out.txt"
expect "the note reaches the prompt" "只审脚本" "$tmp/last-prompt.txt"

echo "a tree with nothing to review"
# From here on the reviewed tree must stay clean, so the output goes to $scratch.
run_clean() {
    set +e
    STUB_MODE="${STUB_MODE:-ok}" PATH="$tmp/bin:$PATH" ./scripts/codex-review.sh "$@" >"$scratch/out.txt" 2>"$scratch/err.txt"
    echo $? > "$scratch/exit.txt"
    set -e
}
exit_file="$scratch/exit.txt"
git add -A >/dev/null 2>&1
git commit -q -m "self-test baseline"
run_clean
check "a clean tree is refused" 65 "Nothing to review" "$scratch/err.txt"

echo "a tree whose final content matches HEAD, but whose index does not"
# `git status --porcelain` reports `AD` here, so the status-based check let this through; the final diff is
# empty and there is nothing for a reviewer to look at.
: > "$scratch/placeholder-ad" 2>/dev/null || true
printf 'x\n' > "$tmp/ad-file.txt"
git add "$tmp/ad-file.txt" >/dev/null 2>&1
rm -f "$tmp/ad-file.txt"
status_before="$(git status --porcelain --untracked-files=normal)"
final_diff="$(git diff HEAD --no-color)"
run_clean
if [[ -n "$status_before" && -z "$final_diff" ]]; then
    check "a staged-then-deleted file is refused as no change" 65 "Nothing to review" "$scratch/err.txt"
else
    # A counterexample that was never set up is not a passing counterexample.
    echo "  FAIL could not stage an AD state (status='${status_before//$'\n'/ }' diff='${final_diff:0:20}')"
    failures=$((failures + 1))
fi
git reset -q HEAD "$tmp/ad-file.txt" >/dev/null 2>&1 || true
run_clean --finish
check "--finish still works on a clean tree" 0 - "$scratch/out.txt"

echo "a design round is asked before there is anything to review"
STUB_MODE=plan run_clean --design
check "the design round is produced" 0 - "$scratch/out.txt"
expect "it is kept as a plan" "PLAN-STATUS: AGREED" .agent-review/last-plan.txt
expect "the design prompt forbids writing the code" "Do NOT write the code" "$tmp/last-prompt.txt"
expect_absent "it does not claim the code changed" "has updated the code" "$tmp/last-prompt.txt"

echo "a design round cannot be answered with a review verdict"
STUB_MODE=ok run_clean --design
check "it is refused" 1 "PLAN-STATUS" "$scratch/err.txt"

echo "--finish takes no round of its own"
STUB_MODE=plan run_clean --finish --design
check "it is refused" 64 - "$scratch/err.txt"

echo "a design round that died is retried as a design round"
STUB_MODE=plan-fail run_clean --design
check "the design round fails" 1 "design round produced no answer" "$scratch/err.txt"
expect_value "the round kind is kept for the retry" "plan" .agent-review/last-round-kind
STUB_MODE=ok run_clean --retry
check "a review verdict cannot answer the retried design round" 1 "PLAN-STATUS" "$scratch/err.txt"
STUB_MODE=plan run_clean --retry
check "the retried design round is answered as one" 0 - "$scratch/out.txt"
expect "and kept as a plan" "PLAN-STATUS: AGREED" .agent-review/last-plan.txt

echo "a refused call does not rewrite what the last round was"
git add -A >/dev/null 2>&1
# A baseline marker, not an assertion: the round files may be byte-identical to the previous commit, and
# `run_clean` leaves errexit on, so a failing commit would end the whole self-test silently.
git commit -q -m "self-test baseline before the refused call" || true
STUB_MODE=ok run_clean
check "the call is refused for having nothing to review" 65 "Nothing to review" "$scratch/err.txt"
expect_value "the recorded kind is untouched" "plan" .agent-review/last-round-kind
# A retry resumes a round that produced no verdict. That design round already answered (PLAN-STATUS), so a
# retry here is refused -- and the refusal must leave the recorded kind alone, which is what this case is for.
STUB_MODE=ok run_clean --retry
check "a retry after a concluded round is refused" 64 "the last round reached one" "$scratch/err.txt"
expect_value "and the recorded kind is still untouched" "plan" .agent-review/last-round-kind

echo "a lost kind record is refused, not guessed"
git add -A >/dev/null 2>&1
git commit -q -m "self-test baseline before the unknown kind" || true
# A retry only resumes a round that produced no verdict, so the state it resumes from has to be one: a round
# that answered would be refused for a different (and correct) reason and never reach the kind record. The round
# needs a change to look at, or the no-change check refuses it before it can produce anything.
: > interrupted.txt
STUB_MODE=unfinished run_clean
check "the interrupted round produced no verdict" 1 "produced no review" "$scratch/err.txt"
rm -f .agent-review/last-round-kind
STUB_MODE=ok run_clean --retry
check "the retry is refused" 64 "Nothing on record" "$scratch/err.txt"

echo "a verdict-shaped message from a failed run is not a concluded round"
# `concluded` is the round's outcome, not the shape of its message: this run prints a valid status line and then
# dies, so it must stay retryable instead of being refused as "the last round reached one".
: > verdict-shaped.txt
STUB_MODE=verdict-then-fail run_clean
check "the round is reported as failed" 1 "exited 1" "$scratch/err.txt"
expect "and recorded as not concluded" "concluded=no" .agent-review/last-status.txt
STUB_MODE=ok run_clean --retry
check "so it can be retried" 0 - "$scratch/out.txt"

echo "an interrupted round does not inherit the previous round's verdict"
# The status record belongs to the round that starts: after a concluded round, an interrupted new round must
# still be retryable.
STUB_MODE=ok run_clean
check "the first round concludes" 0 - "$scratch/out.txt"
: > interrupted-after-success.txt
STUB_MODE=no-completed run_clean
check "the second round produces no conclusion" 1 "produced no review" "$scratch/err.txt"
STUB_MODE=ok run_clean --retry
check "and it is retryable, not refused with the first round's verdict" 0 - "$scratch/out.txt"

echo "a retry after a round that already concluded"
# `--retry` means "resume a round that produced no verdict". Allowing it after a verdict both lies to the
# reviewer (the round was interrupted) and bypasses the no-change refusal on a clean tree.
STUB_MODE=ok run_clean
check "the round concludes" 0 - "$scratch/out.txt"
run_clean --retry
check "a retry after a concluded round is refused" 64 "the last round reached one" "$scratch/err.txt"
run_clean --fresh
check "--fresh still starts a new round" 0 - "$scratch/out.txt"
STUB_MODE=unfinished run_clean --design
check "an interrupted design round produces no verdict" 1 "produced no review" "$scratch/err.txt"
STUB_MODE=plan run_clean --retry --design
check "saying --design resumes it safely" 0 - "$scratch/out.txt"

echo "an archive left by an older task cannot answer for this one"
# The archive keeps rounds from finished tasks; a retry must not read a kind out of it.
git add -A >/dev/null 2>&1
git commit -q -m "self-test baseline before the finished task"
: > finished-task-change.txt
STUB_MODE=ok run_clean
check "a review runs with a change in the tree" 0 - "$scratch/out.txt"
expect_value "its kind is recorded for its own session" "review" .agent-review/last-round-kind
run_clean --finish
check "--finish clears the session" 0 - "$scratch/out.txt"
expect_value "and the kind with it" "" .agent-review/last-round-kind
expect_value "and any verdict record with it" "" .agent-review/last-status.txt
STUB_MODE=ok run_clean --retry
check "a retry then has nothing on record to resume" 64 "Nothing on record" "$scratch/err.txt"

echo "a retry cannot change the kind of round it resumes"
: > switched-kind.txt
STUB_MODE=unfinished run_clean
check "the review produced no verdict" 1 "produced no review" "$scratch/err.txt"
expect_value "so it is recorded as a review round" "review" .agent-review/last-round-kind
STUB_MODE=plan run_clean --retry --design
check "switching a review retry into a design round is refused" 64 "cannot change the kind" "$scratch/err.txt"

echo "an untracked file is a change"
git add -A >/dev/null 2>&1
git commit -q -m "self-test baseline again"
: > untracked-change.txt
run_clean
check "an untracked file is enough to review" 0 - "$scratch/out.txt"

echo "a repository with no commit yet"
# The no-HEAD branch asks the same question as the HEAD path. Concatenating the index and worktree diffs made a
# staged-then-deleted file look like a change; the worktree is compared with the empty tree instead. The helper
# and the stub live outside the repository here, and its own state directory is excluded through
# .git/info/exclude, so the tree really has nothing to review.
norepo="$scratch/no-commit-repo"
mkdir -p "$norepo"
(
    cd "$norepo"
    git init -q .
    git config user.email n@c.i
    git config user.name n
    printf '.agent-review/\n' > .git/info/exclude
    printf 'x\n' > staged.txt
    git add staged.txt
    rm -f staged.txt
    set +e
    STUB_MODE=ok PATH="$tmp/bin:$PATH" "$tmp/scripts/codex-review.sh" >"$scratch/out.txt" 2>"$scratch/err.txt"
    echo $? > "$scratch/exit.txt"
    set -e
)
check "a staged-then-deleted file in an uncommitted repository is refused" 65 "Nothing to review" "$scratch/err.txt"

echo "--retry is exempt from the pre-check"
# It resumes a round that produced no verdict, and that is also the case where the tree legitimately has not
# changed since: the first half here records the interrupted round, the second retries it on a clean tree.
: > interrupted-again.txt
STUB_MODE=unfinished run_clean
check "the interrupted review produced no verdict" 1 "produced no review" "$scratch/err.txt"
git add -A >/dev/null 2>&1
git commit -q -m "self-test baseline once more" || true
run_clean --retry
check "a retry runs even on a clean tree" 0 - "$scratch/out.txt"

echo
if [[ "$failures" -eq 0 ]]; then
    echo "self-test passed"
    exit 0
fi
echo "self-test failed: $failures case(s)"
exit 1
