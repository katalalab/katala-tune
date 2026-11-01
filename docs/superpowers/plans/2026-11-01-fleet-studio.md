# Fleet Studio implementation plan

**Goal:** Add a read-only GPUI entry point that reuses Katala Tune's existing data and makes missing, failed, and stale observations visible.

**Architecture:** Keep data access and view state separate from GPUI. Read the existing node configuration and database through `tune-core`'s read-only APIs; do not initialize the engine or automatically start SSH scans. Query configured local model endpoints using bounded GET requests. Keep sample data explicit and separate from real data.

**Constraints:** Pin GPUI and its dependency lockfile. Do not copy machine-specific data into this public repository. Never execute arbitrary commands, trigger inference, initialize a database, or change machine configuration. Background work must not block the UI.

**Verification:** Test timestamp freshness, newer failures, missing or unreadable data, retired nodes, partial topology and local model endpoint failures. Build and run on the available desktop OS; document any Mac prerequisites and unverified behavior.

1. Create the read-only view model and tests, reusing `nodes::load_config`, `Store::open_read_only`, `last_snapshots`, `probe_error`, and `topology_ui_view`.
2. Add bounded model-list GET requests, an explicitly requested read-only hygiene audit, and explicit sample data.
3. Add the GPUI window with asynchronous loading, search and keyboard navigation, source links, timestamps, and visible failures.
4. Verify the focused tests, the repository's applicable checks, real-data behavior and a native window. Document setup, limitations, updates and how to resume.
