# Readable confirmation implementation plan

> Implement in this isolated worktree; tests use synthetic values only.

**Goal:** Make confirmation disclosures reachable on a constrained display before enabling individually scoped machine summaries or non-loopback model-list requests.

**Architecture:** Keep the existing confirmation and grant nonces. Render a dedicated, width-constrained scrolling confirmation screen with an independent presentation ticket and minimum viewport. Both mouse and keyboard authorization use one gate that checks the current target, window activation, viewport and generation.

**Tech Stack:** Rust 1.99, GPUI 0.2.2, existing Fleet Studio scope and model modules.

**Constraints:** Do not open real DBs, make network requests, launch links, interact with authorization UI, delete existing data, push or create PRs. Build with isolated target and one job; run only reviewed exact synthetic test names.

## Steps

1. Add a synthetic test proving real preparation is currently blocked and confirm the expected failure.
2. Add synthetic cases for viewport thresholds, stale presentation tickets and targets, deactivation, resize, cancellation, keyboard repeats, machine/model grants and sample non-dispatch.
3. Implement the presentation gate and dedicated scrolling screen. Keep machine and model authority independent; preserve loopback, recent-local and existing refresh/revocation paths.
4. Document what each scope reads, repeated refresh, organization permission, cancellation limits, operating instructions and unverified display/OS behavior.
5. Inspect test side effects; run only exact approved synthetic tests, rustfmt check and a single-job offline locked GUI build. Do not manipulate permission UI.
6. Review the diff, stage only intentional files, and create a local commit if verification supports it.