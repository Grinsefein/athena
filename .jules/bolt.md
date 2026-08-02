# Bolt's Performance & UX Journal

## 2026-03-05 - [PWA Installability & Duration Formatting Bug]
**Learning:** Modern browsers reject PWA installability if the manifest contains non-standard `data:` scheme icons or fails schema validation. Additionally, duration formatting can break with `NaN:NaN` if the frontend expects a numeric timestamp but the backend returns a formatted string.
**Action:** Remove `data:` scheme icons from the manifest, structure standard PNG icons with separate `any` and `maskable` purposes, and update the frontend's duration formatting logic to check for pre-formatted strings before processing.
