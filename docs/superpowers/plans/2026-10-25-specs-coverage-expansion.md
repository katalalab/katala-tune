# Specifications coverage expansion

The existing approved Rust collector, SSH aggregation and comparison view remain the design. This extends the same read-only inventory to the six authorized machines, including the explicitly opted-in shared machine.

The private baseline contains 351 component observations and 21 exact catalog matches. Its principal gaps are obsolete macOS USB provider selection, omitted public provider properties, failing Windows monitor WMI providers and missing exact product catalog entries. Machine identities and raw observations stay outside this repository.

## Implementation and acceptance

1. Add synthetic failing regressions for native provider shapes, Windows fallback metadata, contextual missing fields and trusted official links.
2. Extend macOS USB, storage, Wi-Fi, Thunderbolt, audio and security reads. Extend Windows disk, display, OS, network and virtualization security reads using bounded built-in providers.
3. Expand the exact-match catalog using manufacturer sources. Record excerpt-only source access explicitly. Do not infer a module's kit, an enclosure's underlying disk, currently connected displays from cached EDID, or installed settings from product capabilities.
4. Calculate expected fields by category. Settings, OS and firmware use official method documentation; they do not claim exact product specification verification. Rebuild all trusted references at the remote validation boundary.
5. Show additional fields, missing-field totals, product verification totals and source limitations in the existing comparison view.
6. Run focused regressions, the complete repository checks and an independent review. Version the collector and app together. Build and deploy the CLI to all six machines and the app to the Windows console and both Macs. Collect fresh reports, compare coverage and inspect the actual comparison view.

No serial numbers, machine identifiers, addresses, SSIDs or raw EDID cross the normalized report boundary. Native provider errors and cached-data limitations remain visible. Unsupported or unidentifiable items remain explicitly unresolved rather than receiving inferred values.
