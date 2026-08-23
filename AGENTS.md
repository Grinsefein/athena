<!-- BEGIN:nextjs-agent-rules -->
# This is NOT the Next.js you know

This version has breaking changes — APIs, conventions, and file structure may all differ from your training data. Read the relevant guide in `node_modules/next/dist/docs/` before writing any code. Heed deprecation notices.
<!-- END:nextjs-agent-rules -->

<!-- BEGIN:athena-dev-pipeline -->
# Athena – Build & Verify Pipeline

Nach **jeder** Änderung in dieser Reihenfolge verifizieren (alles aus dem Repo-Root):

## 1. Backend (Rust/Axum)
```bash
cargo fmt                                        # Formatierung
cargo clippy --all-targets -- -D warnings        # Lint, MUSS warnungsfrei sein (= make lint)
cargo test                                       # Unit- + Integrationstests
```

## 2. Frontend (Svelte 5 + Vite, Quellen in `frontend/src`)
```bash
make build-frontend                              # vite build
```
**Wichtig:** Der Build schreibt `../frontend.html` (das Plugin benennt `index.html` um).
`frontend.html` im Repo-Root ist ein **committetes Build-Artefakt**, das der Server via
`include_str!` ausliefert – nach Frontend-Änderungen immer neu bauen und mit eincheckgen.

## 3. Versionierung (bei sichtbaren Änderungen / Features)
Vier Stellen synchron bumpen:
1. `Cargo.toml` → `version = "x.y.z"` (Cargo.lock aktualisiert sich beim nächsten `cargo check`)
2. `frontend/package.json` → `"version"` (+ `frontend/package-lock.json`, zwei Einträge)
3. `sw.js` → `CACHE_NAME = 'athena-vN'` hochzählen, damit installierte PWAs neue Assets laden

## 4. E2E-Smoke-Test gegen den laufenden Server (optional, aber empfohlen bei Security-Änderungen)
```bash
ATHENA_PASSWORD='test' PORT=8191 ./target/debug/athena &
curl -sI http://127.0.0.1:8191/ | grep -i content-security-policy   # Security-Header prüfen
curl -s -X POST http://127.0.0.1:8191/api/login \
  -H 'Content-Type: application/json' -d '{"password":"test"}'      # Login → Token
curl -s -X POST http://127.0.0.1:8191/api/logout -H "Authorization: Bearer <TOKEN>"
curl -s -X POST http://127.0.0.1:8191/api/analyze \
  -H 'Content-Type: application/json' -H "Authorization: Bearer <TOKEN>" \
  -d '{"url":"http://192.168.178.40/admin"}'                        # muss InvalidUrl liefern (SSRF-Guard)
kill %1
```

## Nützliches
- Passwort-Hash für `.env` erzeugen: `cargo run --bin hash-password -- '<Passwort>'`
- Logs (`athena.log*`) sind gitignored und werden nie committet
- Tests nutzen Literal-IPs (z. B. `93.184.216.34`) statt Hostnames, damit keine DNS-Lookups entstehen
<!-- END:athena-dev-pipeline -->
