# Docker topology read-only adapter

Explicit local environment opt-in (`docker.enabled`, bounded `contexts`, trusted absolute `program`). Read only a context's endpoint, accept local unix/npipe only, then pin `--host` to that endpoint. Never start a daemon or WSL. Reuse bounded child collection with a cumulative deadline, output/count caps and clean Docker environment.

Collect projected fields only: engine identity, container state/optional health, CPU/memory limits, restart/log policy, mounts, network membership and published ports; network and volume catalog. Hash private identities and paths before typed DTO persistence. Missing health remains unknown; unhealthy exited containers are not current incidents. Network membership is its own edge. Deduplicate by engine identity. Partial/excluded receipts never imply absence.

Use the existing topology lease, fenced database storage, CLI, public export and UI. No schema migration, daemon, SDK or actions. Tests first: local endpoint rejection, privacy projection, null/zero, dedup, limits/timeouts, graph/store/export. Parent performs real canary and review; this worktree does not access live Docker or normal config/database.

Official sources checked: https://docs.docker.com/reference/cli/docker/context/inspect/ ; https://docs.docker.com/reference/cli/docker/inspect/ ; https://docs.docker.com/engine/logging/drivers/local/ . Local driver defaults are bounded; only running json-file missing max-size is an unbounded candidate. Explicit max-size=-1 is preserved as a policy fact.

Additional official policy source: https://docs.docker.com/engine/logging/drivers/json-file/ .

Review correction sources: https://raw.githubusercontent.com/moby/moby/master/daemon/logger/jsonfilelog/jsonfilelog.go and https://raw.githubusercontent.com/docker/go-units/master/size.go . Missing named-volume targets/identities keep only the affected engine partial.

Current mount types verified against https://docs.docker.com/engine/storage/ ; image mounts remain metadata-only and never become volume references. Numeric literal syntax: https://pkg.go.dev/strconv#ParseFloat .
