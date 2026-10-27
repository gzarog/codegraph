//! CodeGraph's anonymous usage telemetry. `protocol` is the wire format and
//! the field allowlist both sides enforce; `db` and `server` are the
//! self-hosted ingest endpoint and dashboard (`codegraph-telemetry-server`).

pub mod db;
pub mod protocol;
pub mod server;
