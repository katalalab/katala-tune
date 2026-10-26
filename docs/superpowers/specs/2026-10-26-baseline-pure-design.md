# Issue #34: pure hourly baseline observations

Pure component on an independent branch based on main17474e1 (parent merged PR #98). Parent permits acceptance definition and pure fixtures, while existing PR stability has priority. Runtime integration requires parent coordination; no renderer/IPC/shared schema/collector edits or heavy Rust/native tests in this slice. No action or effect verification (#35), notification, startup or deployment.

## Decision and limits

A single fleet/global threshold is quick to learn but conflates weekly workload patterns. Use per-node, per-metric UTC weekday/four-hour profiles over the previous28days instead. These are conservative engineering defaults delegated by the user, not user-specified thresholds. This component consumes supplied numeric hourly records and has no database or host access.

Initial supported metrics are cpu,mem,commit percentages. It reports elevated use and increasing memory/commit observations, not host slowness, a memory leak, version causality or action effectiveness. Benchmark regression needs the later snapshot/benchmark contract; no bench data exists in98's15monitor keys. Absolute existing alerts remain independent.

Retained data permits quantiles OF HOURLY MEANS, not quantiles of raw measurements. Daily summaries cannot reconstruct hourly profiles or intra-day slopes and are excluded. Count means represented valid minutes, not continuous sample coverage; no stronger confidence is inferred. UTC profiles are explicit, with no guessed node timezone/DST history.

## Interface and normalization

`analyzeHours(points,{metric,now})` is pure/deterministic. The caller must select one node and one metric with a compatible percentage definition before supplying points; this component has no identity/capability fields and cannot separate mixed nodes or metric definitions. Points are {at,mean,minutes,resolution:'hour'}, at is the UTC bucket start. now must be a safe nonnegative timestamp within JS Date's supported range. A programming-invalid metric/shape throws; data problems return insufficient evidence. Max4096input records; exceeding the limit yields insufficient/input_limit with no finding.

Require hour-aligned at, finite mean in[0,100], integer minutes in[30,60]. Missing, null, Boolean, string, nonfinite, coarse daily, partial and future/current-hour records do not become zero. Use only completed hours with bucket<floor(now/hour). Identical duplicate hours deduplicate; conflicting duplicates make that hour unavailable regardless of input ordering.

## Disjoint windows and maturity

Let end=floor(now/hour). Recent observation window is[end-24h,end); historical training is[end-28d,end-24h). No recent value is trained into its own reference. A profile key is UTC weekday(0..6) and UTC four-hour slot(0..5).

Historical global maturity needs>=72valid hours across>=7distinct UTC dates. A matched profile needs>=8valid historical hours across>=3distinct UTC dates. This can take around three weeks from a cold start; status must expose the actual missing coverage, never describe the waiting period as normal.

Recent evidence needs>=18matched valid hours of24, including>=4in its first6hours and>=4in its last6hours. If any maturity/coverage gate fails, status is insufficient with stable reason codes/coverage and no baseline-derived observations. There is no fallback from sparse profiles to global thresholds.

Quantiles use sorted linear interpolation(position=(n-1)*q); median is q=0.5. For each recent hour, reference is the matching historical profile's median and p90; p10 is also returned for evidence. Elevated use needs>=4of the last6complete hours strictly above that profile's p90+15percentage points.

Memory/commit trend uses residual(value-profile median) over the24hwindow. Require median(last6 residuals)-median(first6 residuals)>=8percentage points and Theil-Sen median of all distinct-time residual pair slopes>=0.25points/hour. Slopes use actual time gaps. CPU trend is explicitly not assessed. A step change can be elevated without a gradual trend; a gradual increase can be rising before the elevation threshold. No commands/actions are produced.

Coverage reason codes are input_limit, history_hours, history_days, recent_coverage, profile_coverage, endpoint_coverage and hourly_data_required. Multiple unmet gates can be reported together. Input-limit rejection leaves unprocessed coverage/evidence null; otherwise counts reflect valid unique nonconflicting hours. Each observation is {kind: elevated|rising, metric}; neither contains an action.

Result: metric,timezone:'UTC',state(insufficient/within_baseline/elevated/rising/elevated_and_rising),reason codes,coverage counts,profile statistics and finite derived evidence. No input identifiers/names/raw objects are echoed. A within_baseline result means only these hourly-use windows were assessed, not system health.

## Deterministic acceptance

-28-day synthetic weekly/daytime schedule stays within baseline; a sustained pressure step becomes elevated and a24hlinear memory rise becomes rising; CPU does not receive a leak/trend verdict.
- Insufficient history, sparse weekday profiles, holes/endpoints, current/future-only and daily-only inputs produce no observation and give an explicit reason.
- Real zero differs from null/Boolean/string; identical/conflicting duplicates are order-independent; actual gaps preserve slope units.
- Recent windows do not contaminate training; input ordering and process timezone do not change results; fixed input cap rejects amplification.
- Pure fixtures and independent review first. Rust parity, saved-data adapter, UI/IPC and operational acceptance remain follow-up after parent coordination and an assigned integration slot.
