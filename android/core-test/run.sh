#!/usr/bin/env bash
# Compiles the platform-independent Android core on a plain JDK and runs its tests.
set -euo pipefail
cd "$(dirname "$0")"
OUT=$(mktemp -d)
javac -Xlint:-options --release 11 -d "$OUT" ../app/src/main/java/dev/ferry/core/*.java CoreTest.java 2>&1 | grep -v -e "^Note:" -e "JAVA_TOOL_OPTIONS" >&2 || true
# --release 11 for core (Android-compatible API surface); test uses JDK 11+ XDH.
exec java -cp "$OUT" CoreTest "$@"
