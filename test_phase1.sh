#!/usr/bin/env bash
set -e
cd "$(dirname "$0")"

BIN=./target/debug/manguesechee-agent

pkill -f manguesechee-agent 2>/dev/null || true
sleep 0.3

RUST_LOG=info "$BIN" --port 24800 >listener.log 2>&1 &
LPID=$!
sleep 0.5

RUST_LOG=info "$BIN" --port 24801 --connect 127.0.0.1:24800 >connector.log 2>&1
sleep 0.3

kill $LPID 2>/dev/null || true
wait $LPID 2>/dev/null || true

echo "=== connector ==="
cat connector.log
echo ""
echo "=== listener ==="
cat listener.log
