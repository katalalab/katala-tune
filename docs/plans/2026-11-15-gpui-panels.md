# GPUI migration: native saved-data panels

The full migration must retain all sixteen existing views, node tabs, live stream ownership, guarded actions, settings, host integration and the updater. Deployment and actual GUI E2E on both desktop targets remain release gates. A saved-data browser alone does not complete that migration.

This increment adds functional native browsers for status, resources, processes, scheduled jobs, logs, inventory, AI usage and sessions, security, specifications, power, topology and action history. Overview and node findings/history remain available. Network diagnostics, pairing, the monitor dashboard, resource live streams, settings and guarded mutations follow in later increments; they must not be presented as working controls before their host contracts are connected.

## Global constraints

- Use tune-core Engine accessors; preserve current privacy and opt-in policies. Do not expose raw database network fields or bypass the log filters.
- Opening a view reads saved data only. It never starts remote collection, recomputation, pairing or a stream. Manual collection is a separate operation.
- Missing, stale, partial, excluded and failed values remain distinct. AI prices are estimates; unknown prices and missing power values are not zero.
- Topology uses the bounded UI projection, separates accepted facts and candidates, and explains omissions and selected-row validation.
- Database work runs off the GPUI thread. Navigation uses request generations so a late response cannot replace the newly selected view. Render long result lists through virtualization.
- Search supports native text input and Japanese IME. Submit explicitly, and report query errors visibly. Bounded results must never imply complete history or a total count.
- Keep preview data separate from the stable application. Do not embed private machine identifiers in source, fixtures or public documentation.

## Execution and evidence

1. Add small presentation models and real Engine loading for the saved-data views. Test missing values, failure/partial states, privacy boundaries and bounded results with synthetic data.
2. Add native navigation, keyboard actions, Unicode/IME search and virtual result rows. Move overview reload work off the UI thread and discard obsolete requests.
3. Exercise startup and the saved-data routes against a dedicated synthetic database. Verify read-only rendering does not add snapshots, events or collection cursors. Perform actual desktop navigation and IME checks when an active display is available.
4. Review the complete diff, run repository checks and supported-platform builds, then publish the reviewed preview increment through a PR and CI artifacts. Record actual platform verification and remaining gaps.

The full goal also requires the remaining host features, complete GUI E2E, rollout to every configured machine and measurement of release size, memory and responsiveness. Unreachable machines stay deferred until the user restores connectivity; no polling is needed while that work is deferred.

## Concurrent main integration

While this increment was under verification, main gained the K-mark branding, packaging script, and node Live/log/status/topology views. Preserve branding, packaging, initial-route flags, optional diagnostics, node Live and log level selection. The new async panels replace the earlier synchronous saved-data renderers. Node Live must stop on navigation/deactivation/release, respect current ledger failures and terminal retry state, and retain missing values. Revalidate the combined tree; earlier results apply only to the pre-merge revision.
