#![forbid(unsafe_code)]

use crate::ssrf::is_host_allowed;
use crate::types::PepError;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::cell::RefCell;
use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

// ── Policy input types (structured input for OPA evaluation) ────────────

#[derive(Debug, Serialize)]
pub struct PolicyInput {
    pub action: ActionInput,
    pub subject: SubjectInput,
    pub context: ContextInput,
}

#[derive(Debug, Serialize)]
pub struct ActionInput {
    #[serde(rename = "type")]
    pub action_type: String,
    pub resource: ResourceInput,
}

#[derive(Debug, Serialize)]
pub struct ResourceInput {
    pub url: String,
    pub host: String,
    pub path: String,
    pub method: String,
    pub scheme: String,
}

#[derive(Debug, Serialize)]
pub struct SubjectInput {
    pub user_id: String,
    pub workspace_id: String,
}

#[derive(Debug, Serialize)]
pub struct ContextInput {
    pub time: String,
    pub stage: String,
    pub mode: String,
}

// ── Policy decision types (structured output from OPA evaluation) ───────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolicyDecision {
    pub allow: bool,
    pub reason: Option<String>,
    pub constraints: Option<Constraints>,
    pub decision_id: String,
    pub policy_hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Constraints {
    pub max_bytes: Option<usize>,
    pub allowed_domains: Option<Vec<String>>,
    pub rate_limit_per_min: Option<u32>,
}

// ── PolicyInput construction helpers ────────────────────────────────────

impl PolicyInput {
    pub fn from_http_url(url: &reqwest::Url, method: &str) -> Self {
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs().to_string())
            .unwrap_or_else(|_| "0".to_string());

        Self {
            action: ActionInput {
                action_type: "http.request".to_string(),
                resource: ResourceInput {
                    url: url.to_string(),
                    host: url.host_str().unwrap_or("").to_lowercase(),
                    path: url.path().to_string(),
                    method: method.to_uppercase(),
                    scheme: url.scheme().to_string(),
                },
            },
            subject: SubjectInput {
                user_id: "default".to_string(),
                workspace_id: "default".to_string(),
            },
            context: ContextInput {
                time: ts,
                stage: "default".to_string(),
                mode: "interactive".to_string(),
            },
        }
    }
}

// ── Evaluator trait (seam for testing) ──────────────────────────────────

pub trait PolicyEvaluator {
    fn evaluate(&self, input: &PolicyInput) -> Result<PolicyDecision, PepError>;
    fn policy_hash(&self) -> &str;
}

// ── NullEvaluator (fallback when no policy directory is configured) ─────

pub struct NullEvaluator {
    allowed_domains: Vec<String>,
}

impl NullEvaluator {
    pub fn new(allowed_domains: Vec<String>) -> Self {
        Self { allowed_domains }
    }
}

impl PolicyEvaluator for NullEvaluator {
    fn evaluate(&self, input: &PolicyInput) -> Result<PolicyDecision, PepError> {
        let host = &input.action.resource.host;
        if !is_host_allowed(host, &self.allowed_domains) {
            return Ok(PolicyDecision {
                allow: false,
                reason: Some("domain not allowlisted".to_string()),
                constraints: None,
                decision_id: Uuid::new_v4().to_string(),
                policy_hash: String::new(),
            });
        }
        Ok(PolicyDecision {
            allow: true,
            reason: Some("domain allowlisted (static)".to_string()),
            constraints: None,
            decision_id: Uuid::new_v4().to_string(),
            policy_hash: String::new(),
        })
    }

    fn policy_hash(&self) -> &str {
        ""
    }
}

// ── RegorusEvaluator (embedded Rego evaluation via regorus) ─────────────

pub struct RegorusEvaluator {
    engine: RefCell<regorus::Engine>,
    hash: String,
}

impl RegorusEvaluator {
    /// Load all `.rego` policy files and `.json` data files from `policy_dir`.
    /// Test files (containing `_test`) are excluded from policy loading.
    pub fn from_dir(policy_dir: &Path) -> Result<Self, PepError> {
        let mut engine = regorus::Engine::new();
        let mut hasher = Sha256::new();

        // Collect and sort .rego files (deterministic hash).
        let mut rego_files: Vec<_> = fs::read_dir(policy_dir)
            .map_err(|e| PepError::Policy(format!("reading policy dir: {e}")))?
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "rego"))
            .filter(|entry| {
                // Skip OPA test files — they are not runtime policy.
                !entry.file_name().to_string_lossy().contains("_test")
            })
            .collect();
        rego_files.sort_by_key(|e| e.file_name());

        if rego_files.is_empty() {
            return Err(PepError::Policy(
                "no .rego files found in policy directory".to_string(),
            ));
        }

        for entry in &rego_files {
            let content = fs::read_to_string(entry.path()).map_err(|e| {
                PepError::Policy(format!("reading {}: {e}", entry.path().display()))
            })?;
            hasher.update(content.as_bytes());
            engine
                .add_policy(entry.file_name().to_string_lossy().to_string(), content)
                .map_err(|e| {
                    PepError::Policy(format!("parsing {}: {e}", entry.path().display()))
                })?;
        }

        // Load .json data files.
        let mut json_files: Vec<_> = fs::read_dir(policy_dir)
            .map_err(|e| PepError::Policy(format!("reading policy dir: {e}")))?
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
            .collect();
        json_files.sort_by_key(|e| e.file_name());

        for entry in &json_files {
            let data = regorus::Value::from_json_file(entry.path().to_string_lossy().as_ref())
                .map_err(|e| {
                    PepError::Policy(format!("loading data {}: {e}", entry.path().display()))
                })?;
            engine.add_data(data).map_err(|e| {
                PepError::Policy(format!("adding data {}: {e}", entry.path().display()))
            })?;
        }

        let hash = format!("{:x}", hasher.finalize());

        Ok(Self {
            engine: RefCell::new(engine),
            hash,
        })
    }
}

impl PolicyEvaluator for RegorusEvaluator {
    fn evaluate(&self, input: &PolicyInput) -> Result<PolicyDecision, PepError> {
        let decision_id = Uuid::new_v4().to_string();
        let input_json = serde_json::to_string(input)?;
        let input_value = regorus::Value::from_json_str(&input_json)
            .map_err(|e| PepError::Policy(format!("building input value: {e}")))?;

        let mut engine = self.engine.borrow_mut();
        engine.set_input(input_value);

        let result = engine
            .eval_rule("data.pep.decision".to_string())
            .map_err(|e| PepError::Policy(format!("evaluating rule: {e}")))?;

        // If the rule evaluates to Undefined, treat as deny.
        if result == regorus::Value::Undefined {
            return Ok(PolicyDecision {
                allow: false,
                reason: Some("policy evaluation returned undefined".to_string()),
                constraints: None,
                decision_id,
                policy_hash: self.hash.clone(),
            });
        }

        let allow = result["allow"] == regorus::Value::from(true);

        let reason = result["reason"]
            .as_string()
            .ok()
            .map(|s| s.as_ref().to_string());

        let constraints = {
            let c = &result["constraints"];
            if *c != regorus::Value::Undefined {
                Some(Constraints {
                    max_bytes: c["max_bytes"].as_i64().ok().map(|n| n as usize),
                    allowed_domains: None,
                    rate_limit_per_min: c["rate_limit_per_min"].as_i64().ok().map(|n| n as u32),
                })
            } else {
                None
            }
        };

        Ok(PolicyDecision {
            allow,
            reason,
            constraints,
            decision_id,
            policy_hash: self.hash.clone(),
        })
    }

    fn policy_hash(&self) -> &str {
        &self.hash
    }
}

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn sample_policy() -> &'static str {
        r#"package pep
import rego.v1

default decision := {
    "allow": false,
    "reason": "denied by default policy",
}

decision := result if {
    input.action.type == "http.request"
    input.action.resource.scheme in {"http", "https"}
    host := input.action.resource.host
    host_allowed(host)
    result := {
        "allow": true,
        "reason": "domain allowlisted",
        "constraints": object.get(data.config, "constraints", {}),
    }
}

host_allowed(host) if {
    some domain in data.config.allowed_domains
    host == domain
}

host_allowed(host) if {
    some domain in data.config.allowed_domains
    endswith(host, concat("", [".", domain]))
}
"#
    }

    fn sample_data() -> &'static str {
        r#"{
  "config": {
    "allowed_domains": ["example.com", "api.openai.com"],
    "constraints": { "max_bytes": 1048576 }
  }
}"#
    }

    fn make_input(host: &str, scheme: &str) -> PolicyInput {
        PolicyInput {
            action: ActionInput {
                action_type: "http.request".to_string(),
                resource: ResourceInput {
                    url: format!("{scheme}://{host}/"),
                    host: host.to_string(),
                    path: "/".to_string(),
                    method: "GET".to_string(),
                    scheme: scheme.to_string(),
                },
            },
            subject: SubjectInput {
                user_id: "test".to_string(),
                workspace_id: "test".to_string(),
            },
            context: ContextInput {
                time: "0".to_string(),
                stage: "test".to_string(),
                mode: "test".to_string(),
            },
        }
    }

    fn setup_evaluator() -> (TempDir, RegorusEvaluator) {
        let dir = TempDir::new().expect("tempdir");
        fs::write(dir.path().join("pep.rego"), sample_policy()).expect("write policy");
        fs::write(dir.path().join("data.json"), sample_data()).expect("write data");
        let eval = RegorusEvaluator::from_dir(dir.path()).expect("from_dir");
        (dir, eval)
    }

    // ── PolicyInput serialization ───────────────────────────────────

    #[test]
    fn policy_input_serializes_action_type_correctly() {
        let input = make_input("example.com", "https");
        let json: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&input).expect("serialize"))
                .expect("parse");
        assert_eq!(json["action"]["type"], "http.request");
        assert_eq!(json["action"]["resource"]["host"], "example.com");
    }

    // ── RegorusEvaluator ────────────────────────────────────────────

    #[test]
    fn regorus_allows_listed_domain() {
        let (_dir, eval) = setup_evaluator();
        let input = make_input("example.com", "https");
        let decision = eval.evaluate(&input).expect("evaluate");
        assert!(decision.allow, "expected allow for example.com");
        assert!(!decision.policy_hash.is_empty());
    }

    #[test]
    fn regorus_allows_subdomain() {
        let (_dir, eval) = setup_evaluator();
        let input = make_input("sub.api.openai.com", "https");
        let decision = eval.evaluate(&input).expect("evaluate");
        assert!(decision.allow, "expected allow for sub.api.openai.com");
    }

    #[test]
    fn regorus_denies_unlisted_domain() {
        let (_dir, eval) = setup_evaluator();
        let input = make_input("evil.com", "https");
        let decision = eval.evaluate(&input).expect("evaluate");
        assert!(!decision.allow, "expected deny for evil.com");
    }

    #[test]
    fn regorus_denies_non_http_scheme() {
        let (_dir, eval) = setup_evaluator();
        let input = make_input("example.com", "ftp");
        let decision = eval.evaluate(&input).expect("evaluate");
        assert!(!decision.allow, "expected deny for ftp scheme");
    }

    #[test]
    fn regorus_returns_constraints() {
        let (_dir, eval) = setup_evaluator();
        let input = make_input("example.com", "https");
        let decision = eval.evaluate(&input).expect("evaluate");
        let constraints = decision.constraints.expect("constraints should be present");
        assert_eq!(constraints.max_bytes, Some(1_048_576));
    }

    #[test]
    fn regorus_decision_has_unique_id() {
        let (_dir, eval) = setup_evaluator();
        let input = make_input("example.com", "https");
        let d1 = eval.evaluate(&input).expect("evaluate");
        let d2 = eval.evaluate(&input).expect("evaluate");
        assert_ne!(d1.decision_id, d2.decision_id);
    }

    #[test]
    fn regorus_policy_hash_is_deterministic() {
        let dir = TempDir::new().expect("tempdir");
        fs::write(dir.path().join("pep.rego"), sample_policy()).expect("write");
        fs::write(dir.path().join("data.json"), sample_data()).expect("write");
        let e1 = RegorusEvaluator::from_dir(dir.path()).expect("from_dir");
        let e2 = RegorusEvaluator::from_dir(dir.path()).expect("from_dir");
        assert_eq!(e1.policy_hash(), e2.policy_hash());
    }

    #[test]
    fn regorus_rejects_empty_policy_dir() {
        let dir = TempDir::new().expect("tempdir");
        let result = RegorusEvaluator::from_dir(dir.path());
        assert!(result.is_err());
    }

    // ── NullEvaluator ───────────────────────────────────────────────

    #[test]
    fn null_evaluator_allows_listed_domain() {
        let eval = NullEvaluator::new(vec!["example.com".to_string()]);
        let input = make_input("example.com", "https");
        let decision = eval.evaluate(&input).expect("evaluate");
        assert!(decision.allow);
    }

    #[test]
    fn null_evaluator_denies_unlisted_domain() {
        let eval = NullEvaluator::new(vec!["example.com".to_string()]);
        let input = make_input("evil.com", "https");
        let decision = eval.evaluate(&input).expect("evaluate");
        assert!(!decision.allow);
    }
}
