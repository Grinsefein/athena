#!/usr/bin/env bash
set -euo pipefail

usage() {
	printf 'Usage: %s [output.md]\n\nKombiniert alle Quelldateien (kein Build/Generated) in eine Markdown-Datei.\nStandard-Ausgabe: athena-sources.md im Repo-Root.\n' "${0##*/}"
}

case "${1:-}" in
-h | --help)
	usage
	exit 0
	;;
esac

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="${1:-$ROOT/athena-sources.md}"
cd "$ROOT"

DATE="$(date '+%Y-%m-%d %H:%M %Z')"
GIT_SHA="$(git rev-parse --short HEAD 2>/dev/null || echo unbekannt)"

FILES=()
add() {
	local f
	for f in "$@"; do
		[ -f "$f" ] && FILES+=("$f")
	done
}

add README.md AGENTS.md Makefile .gitignore .env.example manifest.json sw.js Cargo.toml
while IFS= read -r -d '' f; do FILES+=("$f"); done < <(find src tests -type f -name '*.rs' -print0 | sort -z)
add frontend/index.html frontend/package.json frontend/vite.config.js frontend/svelte.config.js
while IFS= read -r -d '' f; do FILES+=("$f"); done < <(find frontend/src -type f -print0 | sort -z)
while IFS= read -r -d '' f; do FILES+=("$f"); done < <(find scripts -type f -name '*.sh' ! -name 'dump-source.sh' -print0 | sort -z)

is_text() {
	local f=$1
	[ -f "$f" ] || return 1
	[ -s "$f" ] || return 0
	grep -Iq '' "$f"
}

CLEAN=()
SKIPPED=()
for f in "${FILES[@]}"; do
	if is_text "$f"; then CLEAN+=("$f"); else SKIPPED+=("$f"); fi
done

if [ "${#CLEAN[@]}" -eq 0 ]; then
	echo "Fehler: keine Quelldateien gefunden." >&2
	exit 1
fi

lang_of() {
	case "$1" in
	*.rs) echo rust ;;
	*.js) echo javascript ;;
	*.svelte) echo svelte ;;
	*.css) echo css ;;
	*.html) echo html ;;
	*.json) echo json ;;
	*.toml) echo toml ;;
	*.sh) echo bash ;;
	*.md) echo markdown ;;
	Makefile) echo make ;;
	*) echo text ;;
	esac
}

fence_for() {
	local max len
	max=$(awk '{n=0; while (substr($0, n+1, 1) == "`") n++; if (n > m) m = n} END {print m + 0}' "$1")
	len=$((max >= 3 ? max + 1 : 3))
	printf '%*s' "$len" '' | tr ' ' '`'
}

anchor_of() {
	printf '%s' "$1" | tr '[:upper:]' '[:lower:]' | sed 's/[^a-z0-9_-]//g'
}

TMP="$OUT.tmp"
{
	echo "# Athena – Quellcode"
	echo
	echo "Erzeugt: $DATE · Commit: \`$GIT_SHA\` · Dateien: ${#CLEAN[@]}"
	if [ "${#SKIPPED[@]}" -gt 0 ]; then
		echo
		echo "Übersprungen (binär/leer): ${SKIPPED[*]}"
	fi
	echo
	echo "## Rekonstruktion"
	echo
	echo "1. Dateien unter den jeweils angegebenen Pfaden anlegen"
	echo "2. \`.env\` aus \`.env.example\` erstellen, Passwort-Hash generieren: \`cargo run --bin hash-password -- '<Passwort>'\`"
	echo "3. \`cargo build\`"
	echo "4. Frontend bauen: \`make build-frontend\` (erzeugt \`frontend.html\`, das vom Server eingebettet wird)"
	echo
	echo "Nicht enthalten (generiert oder binär): \`frontend.html\`, Lockfiles (\`Cargo.lock\`, \`package-lock.json\`, \`pnpm-lock.yaml\`), \`app-icon.png\`, Logs, \`.env\`."
	echo
	echo "## Inhalt"
	echo
	for f in "${CLEAN[@]}"; do
		printf -- '- [`%s`](#%s)\n' "$f" "$(anchor_of "$f")"
	done
	echo
	for f in "${CLEAN[@]}"; do
		fence=$(fence_for "$f")
		lang=$(lang_of "$f")
		echo "## $f"
		echo
		echo "$fence$lang"
		cat "$f"
		[ -n "$(tail -c 1 "$f")" ] && echo
		echo "$fence"
		echo
	done
} >"$TMP"

mv "$TMP" "$OUT"
printf '%s (%d Dateien, %s)\n' "$OUT" "${#CLEAN[@]}" "$(du -h "$OUT" | cut -f1)"
