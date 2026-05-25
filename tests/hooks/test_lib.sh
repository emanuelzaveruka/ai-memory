#!/bin/sh
# Smoke tests for hooks/_lib.sh. Run from the repo root:
#
#   sh tests/hooks/test_lib.sh
#
# Exits non-zero on any failure. POSIX shell + sed only, so no extra
# CI setup needed.
set -eu

# shellcheck source=../../hooks/_lib.sh
. "$(dirname "$0")/../../hooks/_lib.sh"

PASS=0
FAIL=0
TMP=$(mktemp -d)
# Pin HOME inside the temp tree so walk-up never leaves the sandbox.
ORIG_HOME=${HOME:-}
HOME="$TMP"
export HOME
trap 'rm -rf "$TMP"; HOME=$ORIG_HOME' EXIT

assert_eq() {
    desc="$1"; want="$2"; got="$3"
    if [ "$want" = "$got" ]; then
        PASS=$((PASS+1))
        printf '  ok  %s\n' "$desc"
    else
        FAIL=$((FAIL+1))
        printf '  FAIL %s\n    want=%s\n    got =%s\n' "$desc" "$want" "$got"
    fi
}

# --- parse_toml_key ---------------------------------------------------
cat >"$TMP/sample.toml" <<EOF
# Comment line
workspace = "movvia"
project = "pe-portais"

# Trailing comment
EOF

assert_eq "parse workspace"           "movvia"     "$(ai_memory_parse_toml_key "$TMP/sample.toml" workspace)"
assert_eq "parse project"             "pe-portais" "$(ai_memory_parse_toml_key "$TMP/sample.toml" project)"
assert_eq "absent key returns empty"  ""           "$(ai_memory_parse_toml_key "$TMP/sample.toml" missing)"
assert_eq "absent file returns empty" ""           "$(ai_memory_parse_toml_key "$TMP/no-such-file.toml" workspace)"

# --- find_marker ------------------------------------------------------
mkdir -p "$TMP/a/b/c/d"
printf 'workspace = "deep"\n' >"$TMP/a/.ai-memory.toml"
assert_eq "walks up to find marker" "$TMP/a/.ai-memory.toml" \
    "$(ai_memory_find_marker "$TMP/a/b/c/d")"
assert_eq "no marker returns empty" "" \
    "$(ai_memory_find_marker "$TMP/nonexistent/path")"

# --- extract_cwd ------------------------------------------------------
PAYLOAD='{"session_id":"x","cwd":"/home/u/foo","tool":"Read"}'
assert_eq "extract cwd from payload"     "/home/u/foo" "$(ai_memory_extract_cwd "$PAYLOAD")"
assert_eq "extract cwd from empty json"  ""            "$(ai_memory_extract_cwd '{}')"
PAYLOAD_NESTED='{"session_id":"x","cwd":"/home/u/root","tool_input":{"cwd":"/tmp/nested"}}'
assert_eq "extract cwd prefers first match" "/home/u/root" "$(ai_memory_extract_cwd "$PAYLOAD_NESTED")"

# --- marker_qs --------------------------------------------------------
QS=$(ai_memory_marker_qs "$TMP/a/b/c")
assert_eq "marker_qs single key" "&cwd=$TMP/a/b/c&workspace=deep" "$QS"

printf 'workspace = "ws1"\nproject = "p1"\n' >"$TMP/a/b/.ai-memory.toml"
QS2=$(ai_memory_marker_qs "$TMP/a/b/c")
assert_eq "closer marker wins" "&cwd=$TMP/a/b/c&workspace=ws1&project=p1" "$QS2"

QS3=$(ai_memory_marker_qs "$TMP/nonexistent")
assert_eq "no marker -> empty qs" "" "$QS3"

# --- url_encode -------------------------------------------------------
assert_eq "url_encode passes safe slug"   "movvia" "$(ai_memory_url_encode "movvia")"
assert_eq "url_encode escapes ampersand"  "a%26b"  "$(ai_memory_url_encode "a&b")"
assert_eq "url_encode escapes equals"     "a%3Db"  "$(ai_memory_url_encode "a=b")"
assert_eq "url_encode escapes plus"       "a%2Bb"  "$(ai_memory_url_encode "a+b")"

# --- summary ----------------------------------------------------------
printf '\n%d passed, %d failed\n' "$PASS" "$FAIL"
[ "$FAIL" -eq 0 ]
