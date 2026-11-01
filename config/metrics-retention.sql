-- Dedicated numeric monitor history. Shared by Rust and the compatibility runtime.
CREATE TABLE IF NOT EXISTS metrics_history (
  node_id TEXT NOT NULL CHECK(length(CAST(node_id AS BLOB)) BETWEEN 1 AND 256),
  resolution TEXT NOT NULL CHECK(resolution IN ('hour', 'day')),
  bucket INTEGER NOT NULL CHECK(bucket >= 0 AND bucket <= 9007199254740991),
  v TEXT NOT NULL CHECK(json_valid(v) AND json_type(v) = 'object' AND length(CAST(v AS BLOB)) BETWEEN 3 AND 2048),
  PRIMARY KEY(node_id, resolution, bucket)
) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS metrics_history_time ON metrics_history(resolution, bucket, node_id);
CREATE TABLE IF NOT EXISTS metrics_retention_state (
  singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
  sealed_before INTEGER NOT NULL DEFAULT 0,
  last_now INTEGER NOT NULL DEFAULT 0,
  late_rows INTEGER NOT NULL DEFAULT 0,
  omitted_rows INTEGER NOT NULL DEFAULT 0,
  expired_rows INTEGER NOT NULL DEFAULT 0,
  capacity_rows INTEGER NOT NULL DEFAULT 0,
  max_rows INTEGER NOT NULL DEFAULT 50000,
  max_bytes INTEGER NOT NULL DEFAULT 16777216
);
INSERT OR IGNORE INTO metrics_retention_state(singleton) VALUES (1);
