#!/usr/bin/env bash
set -euo pipefail

usage() {
	printf 'Usage: %s <x.y.z>\n\nBumps the project version in all four places that must stay in sync\n(see AGENTS.md): Cargo.toml, frontend/package.json, frontend/package-lock.json\n(two entries) and the sw.js cache name (auto-incremented).\n' "${0##*/}"
}

if [ "${1:-}" = "-h" ] || [ "${1:-}" = "--help" ]; then
	usage
	exit 0
fi

NEW_VERSION="${1:-}"
if ! [[ "$NEW_VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
	usage >&2
	exit 1
fi

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

OLD_CARGO=$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -n 1)
[ -n "$OLD_CARGO" ] || { echo "ERROR: could not read version from Cargo.toml" >&2; exit 1; }

if [ "$NEW_VERSION" = "$OLD_CARGO" ]; then
	echo "ERROR: version $NEW_VERSION is already current." >&2
	exit 1
fi

OLD_CACHE=$(sed -n "s/^const CACHE_NAME = 'athena-v\([0-9]*\)';$/\1/p" sw.js)
[ -n "$OLD_CACHE" ] || { echo "ERROR: could not read CACHE_NAME from sw.js" >&2; exit 1; }
NEW_CACHE=$((OLD_CACHE + 1))

sed -i "0,/^version = \"$OLD_CARGO\"$/s//version = \"$NEW_VERSION\"/" Cargo.toml
sed -i "s/\"version\": \"$OLD_CARGO\"/\"version\": \"$NEW_VERSION\"/g" frontend/package.json frontend/package-lock.json
sed -i "s/const CACHE_NAME = 'athena-v$OLD_CACHE';/const CACHE_NAME = 'athena-v$NEW_CACHE';/" sw.js

echo "✓ Cargo.toml:                $OLD_CARGO -> $NEW_VERSION"
echo "✓ frontend/package.json:     $OLD_CARGO -> $NEW_VERSION"
echo "✓ frontend/package-lock.json: 2 entries -> $NEW_VERSION"
echo "✓ sw.js:                     athena-v$OLD_CACHE -> athena-v$NEW_CACHE"
echo ""
echo "Note: run 'cargo check' (or any cargo command) to refresh Cargo.lock."
