use crate::config::PepConfig;
use crate::policy::PolicyDecision;
use crate::types::HttpRequest;
use serde::Serialize;
use std::fs::OpenOptions;
use std::io::Write;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Serialize)]
pub struct AuditEntry {
    pub ts_unix_ms: u64,
    pub method: String,
    pub url: String,
    pub status: u16,
    pub error_code: Option<String>,
    pub request_bytes: usize,
    pub response_bytes: usize,
    pub redirects: u32,
    pub decision: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub policy_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decision_id: Option<String>,
}

#[allow(clippy::too_many_arguments)]
pub fn append_audit_entry(
    config: &PepConfig,
    request: &HttpRequest,
    url: String,
    status: u16,
    error_code: Option<&str>,
    request_bytes: usize,
    response_bytes: usize,
    redirects: u32,
    policy_decision: Option<&PolicyDecision>,
) {
    let ts_unix_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|dur| dur.as_millis() as u64)
        .unwrap_or(0);

    let decision = if error_code.is_some() {
        "deny".to_string()
    } else {
        "allow".to_string()
    };

    let entry = AuditEntry {
        ts_unix_ms,
        method: request.method.clone(),
        url,
        status,
        error_code: error_code.map(|code| code.to_string()),
        request_bytes,
        response_bytes,
        redirects,
        decision,
        policy_hash: policy_decision.map(|d| d.policy_hash.clone()),
        decision_id: policy_decision.map(|d| d.decision_id.clone()),
    };

    if let Ok(line) = serde_json::to_string(&entry)
        && let Ok(mut file) = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&config.audit_log_path)
    {
        let _ = writeln!(file, "{line}");
    }
}
