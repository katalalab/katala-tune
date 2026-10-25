# Machine specs follow-up: test repair, Mac GUI, fleet CLI

The user authorized fixing the remaining tests, updating the Mac GUI and progressing detailed fleet collection and version/CLI management.

1. Reproduce the Windows shared-artifact fixture failure. Correct its temporary-directory volume assumption, preserving process ownership, exit-policy, lease and executable-identity assertions. Run the failing test and the complete repository check.
2. Extend the compatible upstream CLI discovery/help code, keeping existing commands stable. Add version aliases, structured version and executable hash, read-only saved specs export and a no-DB fleet version audit. Test command discovery and invalid requests before state creation.
3. Build a single `0.6.5-specs.2` revision for each platform, record artifact hashes, update Windows application/CLI and both existing Mac application bundles with backups and original preferences intact. Use the existing private ledger and fixed SSH commands. Mac packaging runs independently in the isolated workspace.
4. Refresh detailed reports and audit versions for authorized machines; render real Tauri comparison and inspect Mac launch/bundle evidence. Shared nodes stay excluded unless explicit user permission is received and recorded through a specs-only opt-in. Do not change unrelated shared-machine policy or add any optimization action.

Independent work is delegated under dispatching-parallel-agents: one agent owns Mac packaging/deployment, another CLI discovery/main/tests/docs. Root owns Windows fixture, core fleet-version audit, optional specs read opt-in, final version synchronization and validation. No source/main branch is overwritten on other machines.

Acceptance evidence: full check exit code, Windows and Mac build/version/hash records, CLI command tests and actual fleet audit, detailed reports/missing fields, GUI launch/comparison. Private identifiers and artifacts remain outside the repository; OSS HEAD and reachable-history scans must pass.
