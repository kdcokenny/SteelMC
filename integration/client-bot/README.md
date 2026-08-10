# Protocol-776 exploration client

This standalone crate pins Azalea commit `6249c295d353b9b3ef68f665b311cba39211fd19` (`0.16.0+mc26.2`). It joins an offline-mode test server, requests a configurable view distance, observes spawn/chunk/keepalive events, issues deterministic teleport commands, and emits one JSON result. The test account must be allowed to use `/teleport` (the E2E harness installs the fixed offline UUID for `SteelProbe`).

```bash
cargo build --release --locked
STEEL_CLIENT_ADDRESS=127.0.0.1:25585 \
STEEL_CLIENT_DWELL_TICKS=200 \
STEEL_CLIENT_WAYPOINTS='0,0;512,0;512,512;0,512' \
./target/release/steel-worldgen-client-bot
```

Configuration: `STEEL_CLIENT_USERNAME` (default `SteelProbe`), `STEEL_CLIENT_VIEW_DISTANCE` (4), `STEEL_CLIENT_DWELL_TICKS` (100), `STEEL_CLIENT_MIN_CHUNKS_PER_PHASE` (9), and semicolon-separated `STEEL_CLIENT_WAYPOINTS`. Run only against an isolated test server; the harness uses offline mode and explicit EULA acceptance.
