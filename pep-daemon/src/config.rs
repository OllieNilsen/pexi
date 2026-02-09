use std::env;
use std::path::PathBuf;

#[derive(Clone, Debug)]
pub struct PepConfig {
    pub allowed_domains: Vec<String>,
    pub max_request_bytes: usize,
    pub max_response_bytes: usize,
    pub max_redirects: u32,
    pub audit_log_path: PathBuf,
}

impl PepConfig {
    pub fn from_env() -> Self {
        let allowed_domains = env::var("PEP_ALLOWED_DOMAINS")
            .ok()
            .map(|raw| {
                raw.split(',')
                    .map(|entry| entry.trim().to_lowercase())
                    .filter(|entry| !entry.is_empty())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        let max_request_bytes = env::var("PEP_MAX_REQUEST_BYTES")
            .ok()
            .and_then(|raw| raw.parse::<usize>().ok())
            .unwrap_or(5 * 1024 * 1024);

        let max_response_bytes = env::var("PEP_MAX_RESPONSE_BYTES")
            .ok()
            .and_then(|raw| raw.parse::<usize>().ok())
            .unwrap_or(10 * 1024 * 1024);

        let max_redirects = env::var("PEP_MAX_REDIRECTS")
            .ok()
            .and_then(|raw| raw.parse::<u32>().ok())
            .unwrap_or(5);

        let audit_log_path = env::var("PEP_AUDIT_LOG")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("audit.jsonl"));

        Self {
            allowed_domains,
            max_request_bytes,
            max_response_bytes,
            max_redirects,
            audit_log_path,
        }
    }
}
