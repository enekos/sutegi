#!/bin/sh
# Integration test for sutegi-pgext extensions against a real PostgreSQL.
#
#   scripts/pgext-test.sh [pg_config] [project ...]   (default: examples/pgext-demo)
#
# Builds each project with `sutegi pgext build`, installs it into the server
# pg_config points at (set PGEXT_SUDO=sudo when its directories need root),
# starts a throwaway cluster and runs every test*.sql in its own database with
# ON_ERROR_STOP: tests assert by raising. A file named test-latin1*.sql runs
# in a LATIN1 database. Any TRAP:/PANIC:/crashed backend/Rust-unwind warning
# in the server log fails the run.
set -eu

PG_CONFIG=${1:-pg_config}
[ $# -gt 0 ] && shift
[ $# -gt 0 ] || set -- examples/pgext-demo
SUDO=${PGEXT_SUDO:-}
PORT=${PGEXT_PORT:-5497}
BIN=$("$PG_CONFIG" --bindir)
ROOT=$(cd "$(dirname "$0")/.." && pwd)
TMP=$(mktemp -d)

cleanup() {
	"$BIN/pg_ctl" -D "$TMP/data" -m immediate stop >/dev/null 2>&1 || true
	rm -rf "$TMP"
}
trap cleanup EXIT INT TERM

cd "$ROOT"

for project in "$@"; do
	out=$(cargo run -q -p sutegi-cli -- pgext build "$project" --pg-config "$PG_CONFIG")
	echo "$out"
	work=${out##* in }
	$SUDO make -s -C "$work" with_llvm=no install
done

"$BIN/initdb" -D "$TMP/data" -U postgres --encoding=UTF8 --locale=C >/dev/null
"$BIN/pg_ctl" -D "$TMP/data" -l "$TMP/server.log" -w \
	-o "-p $PORT -k $TMP -c listen_addresses=''" start >/dev/null

failed=0
n=0
for project in "$@"; do
	for test in "$project"/test*.sql; do
		[ -e "$test" ] || continue
		n=$((n + 1))
		db=t$n
		case $(basename "$test") in
		test-latin1*) "$BIN/createdb" -h "$TMP" -p "$PORT" -U postgres -E LATIN1 -T template0 --locale=C "$db" ;;
		*) "$BIN/createdb" -h "$TMP" -p "$PORT" -U postgres "$db" ;;
		esac
		if "$BIN/psql" -X -q -h "$TMP" -p "$PORT" -U postgres -d "$db" \
			-v ON_ERROR_STOP=1 -c 'SET client_min_messages = warning' -f "$test"; then
			echo "ok   $test"
		else
			echo "FAIL $test"
			failed=1
		fi
	done
done

if grep -E 'TRAP:|PANIC:|terminated by signal|was terminated|unwound Rust frames' "$TMP/server.log"; then
	echo "FAIL server log shows a crash"
	failed=1
fi
[ $failed -eq 0 ] || { echo "--- server log"; tail -50 "$TMP/server.log"; exit 1; }
echo "all $n test file(s) passed"
