use serde::{Deserialize, Serialize};
use std::io;
use thiserror::Error;

#[derive(Debug, Serialize, Deserialize)]
pub struct HttpRequest {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body_base64: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct HttpResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body_base64: Option<String>,
    pub error: Option<ErrorEnvelope>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ErrorEnvelope {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Error)]
pub enum PepError {
    #[error("io error: {0}")]
    Io(#[from] io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("policy error: {0}")]
    Policy(String),
}

pub fn error_response(code: &str, message: &str) -> HttpResponse {
    HttpResponse {
        status: 0,
        headers: Vec::new(),
        body_base64: None,
        error: Some(ErrorEnvelope {
            code: code.to_string(),
            message: message.to_string(),
        }),
    }
}
