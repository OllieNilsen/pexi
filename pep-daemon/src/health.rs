use crate::config::PepConfig;
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct HealthStatus {
    pub status: &'static str,
    pub version: &'static str,
    pub allowed_domains_count: usize,
    pub max_request_bytes: usize,
    pub max_response_bytes: usize,
}

/// Build a health status snapshot from the current config.
pub fn health_check(config: &PepConfig) -> HealthStatus {
    HealthStatus {
        status: "ok",
        version: env!("CARGO_PKG_VERSION"),
        allowed_domains_count: config.allowed_domains.len(),
        max_request_bytes: config.max_request_bytes,
        max_response_bytes: config.max_response_bytes,
    }
}
