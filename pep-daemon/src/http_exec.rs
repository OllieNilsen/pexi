use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use bytes::Bytes;
use reqwest::Method;
use reqwest::Url;
use reqwest::blocking::Client;
use std::io::Read;

use crate::audit::append_audit_entry;
use crate::config::PepConfig;
use crate::ssrf::{ensure_public_host, is_host_allowed, is_scheme_allowed};
use crate::types::{HttpRequest, HttpResponse, PepError, error_response};

pub fn execute_request(
    client: &Client,
    request: HttpRequest,
    config: &PepConfig,
) -> Result<HttpResponse, PepError> {
    let method: Method = match request.method.parse() {
        Ok(method) => method,
        Err(_) => {
            let response = error_response("invalid_method", "invalid HTTP method");
            append_audit_entry(
                config,
                &request,
                sanitize_url_string(&request.url),
                0,
                Some("invalid_method"),
                0,
                0,
                0,
            );
            return Ok(response);
        }
    };
    let mut url = match Url::parse(&request.url) {
        Ok(parsed) => parsed,
        Err(err) => {
            let response = error_response("invalid_url", &err.to_string());
            append_audit_entry(
                config,
                &request,
                sanitize_url_string(&request.url),
                0,
                Some("invalid_url"),
                0,
                0,
                0,
            );
            return Ok(response);
        }
    };

    if !is_scheme_allowed(url.scheme()) {
        let response = error_response("invalid_url", "unsupported URL scheme");
        append_audit_entry(
            config,
            &request,
            sanitize_url(&url),
            0,
            Some("invalid_url"),
            0,
            0,
            0,
        );
        return Ok(response);
    }

    let host = match url.host_str() {
        Some(host) => host.to_lowercase(),
        None => {
            let response = error_response("invalid_url", "missing host");
            append_audit_entry(
                config,
                &request,
                sanitize_url(&url),
                0,
                Some("invalid_url"),
                0,
                0,
                0,
            );
            return Ok(response);
        }
    };

    if !is_host_allowed(&host, &config.allowed_domains) {
        let response = error_response("denied_by_policy", "domain not allowlisted");
        append_audit_entry(
            config,
            &request,
            sanitize_url(&url),
            0,
            Some("denied_by_policy"),
            0,
            0,
            0,
        );
        return Ok(response);
    }

    if let Err(err) = ensure_public_host(&url) {
        let response = error_response("ssrf_blocked", &err);
        append_audit_entry(
            config,
            &request,
            sanitize_url(&url),
            0,
            Some("ssrf_blocked"),
            0,
            0,
            0,
        );
        return Ok(response);
    }

    let body_bytes = if let Some(body_base64) = request.body_base64.as_ref() {
        let body = match BASE64.decode(body_base64.as_str()) {
            Ok(body) => body,
            Err(err) => {
                let response = error_response("invalid_body", &format!("base64 decode: {err}"));
                append_audit_entry(
                    config,
                    &request,
                    sanitize_url(&url),
                    0,
                    Some("invalid_body"),
                    0,
                    0,
                    0,
                );
                return Ok(response);
            }
        };
        if body.len() > config.max_request_bytes {
            let response = error_response("constraint_violation", "request body exceeds max bytes");
            append_audit_entry(
                config,
                &request,
                sanitize_url(&url),
                0,
                Some("constraint_violation"),
                0,
                0,
                0,
            );
            return Ok(response);
        }
        Some(Bytes::from(body))
    } else {
        None
    };
    let request_bytes = body_bytes.as_ref().map(|body| body.len()).unwrap_or(0);

    let mut redirects = 0;
    loop {
        let mut builder = client.request(method.clone(), url.clone());
        for (key, value) in &request.headers {
            builder = builder.header(key, value);
        }
        if let Some(body) = &body_bytes {
            builder = builder.body(body.clone());
        }

        let response = match builder.send() {
            Ok(resp) => resp,
            Err(err) => {
                let error = error_response("http_error", &err.to_string());
                append_audit_entry(
                    config,
                    &request,
                    sanitize_url(&url),
                    0,
                    Some("http_error"),
                    request_bytes,
                    0,
                    redirects,
                );
                return Ok(error);
            }
        };

        if response.status().is_redirection() {
            if redirects >= config.max_redirects {
                let error = error_response("redirect_blocked", "redirect limit exceeded");
                append_audit_entry(
                    config,
                    &request,
                    sanitize_url(&url),
                    response.status().as_u16(),
                    Some("redirect_blocked"),
                    request_bytes,
                    0,
                    redirects,
                );
                return Ok(error);
            }

            let location = match response.headers().get(reqwest::header::LOCATION) {
                Some(loc) => loc.to_str().unwrap_or_default().to_string(),
                None => {
                    let error = error_response("redirect_blocked", "missing Location header");
                    append_audit_entry(
                        config,
                        &request,
                        sanitize_url(&url),
                        response.status().as_u16(),
                        Some("redirect_blocked"),
                        request_bytes,
                        0,
                        redirects,
                    );
                    return Ok(error);
                }
            };

            let next_url = match url.join(&location) {
                Ok(next) => next,
                Err(_) => {
                    let error = error_response("redirect_blocked", "invalid redirect URL");
                    append_audit_entry(
                        config,
                        &request,
                        sanitize_url(&url),
                        response.status().as_u16(),
                        Some("redirect_blocked"),
                        request_bytes,
                        0,
                        redirects,
                    );
                    return Ok(error);
                }
            };

            if next_url.scheme() != url.scheme() {
                let error = error_response("redirect_blocked", "scheme change blocked");
                append_audit_entry(
                    config,
                    &request,
                    sanitize_url(&url),
                    response.status().as_u16(),
                    Some("redirect_blocked"),
                    request_bytes,
                    0,
                    redirects,
                );
                return Ok(error);
            }

            let next_host = match next_url.host_str() {
                Some(host) => host.to_lowercase(),
                None => {
                    let error = error_response("redirect_blocked", "redirect missing host");
                    append_audit_entry(
                        config,
                        &request,
                        sanitize_url(&url),
                        response.status().as_u16(),
                        Some("redirect_blocked"),
                        request_bytes,
                        0,
                        redirects,
                    );
                    return Ok(error);
                }
            };

            if !is_host_allowed(&next_host, &config.allowed_domains) {
                let error = error_response("redirect_blocked", "redirect domain not allowlisted");
                append_audit_entry(
                    config,
                    &request,
                    sanitize_url(&url),
                    response.status().as_u16(),
                    Some("redirect_blocked"),
                    request_bytes,
                    0,
                    redirects,
                );
                return Ok(error);
            }

            if let Err(err) = ensure_public_host(&next_url) {
                let error = error_response("ssrf_blocked", &err);
                append_audit_entry(
                    config,
                    &request,
                    sanitize_url(&url),
                    response.status().as_u16(),
                    Some("ssrf_blocked"),
                    request_bytes,
                    0,
                    redirects,
                );
                return Ok(error);
            }

            redirects += 1;
            url = next_url;
            continue;
        }

        let status = response.status().as_u16();
        let headers = response
            .headers()
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or_default().to_string()))
            .collect::<Vec<_>>();

        let body = match read_body_with_cap(response, config.max_response_bytes) {
            Ok(bytes) => bytes,
            Err(err) => {
                let error = error_response("constraint_violation", &err);
                append_audit_entry(
                    config,
                    &request,
                    sanitize_url(&url),
                    status,
                    Some("constraint_violation"),
                    request_bytes,
                    0,
                    redirects,
                );
                return Ok(error);
            }
        };

        append_audit_entry(
            config,
            &request,
            sanitize_url(&url),
            status,
            None,
            request_bytes,
            body.len(),
            redirects,
        );

        return Ok(HttpResponse {
            status,
            headers,
            body_base64: Some(BASE64.encode(body)),
            error: None,
        });
    }
}

fn read_body_with_cap(
    mut response: reqwest::blocking::Response,
    cap: usize,
) -> Result<Vec<u8>, String> {
    read_with_cap(&mut response, cap)
}

pub fn read_with_cap<R: Read>(reader: &mut R, cap: usize) -> Result<Vec<u8>, String> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        let read = reader
            .read(&mut chunk)
            .map_err(|err| format!("read error: {err}"))?;
        if read == 0 {
            break;
        }
        if buf.len() + read > cap {
            return Err("response body exceeds max bytes".to_string());
        }
        buf.extend_from_slice(&chunk[..read]);
    }
    Ok(buf)
}

pub fn sanitize_url(url: &Url) -> String {
    let mut sanitized = url.clone();
    sanitized.set_query(None);
    sanitized.set_fragment(None);
    sanitized.to_string()
}

pub fn sanitize_url_string(raw: &str) -> String {
    let trimmed = raw.split('#').next().unwrap_or(raw);
    trimmed.split('?').next().unwrap_or(trimmed).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn read_with_cap_rejects_oversized_body() {
        let payload = vec![1u8; 10];
        let mut cursor = Cursor::new(payload);
        let err = read_with_cap(&mut cursor, 5).expect_err("expected cap error");
        assert!(err.contains("exceeds max bytes"));
    }

    #[test]
    fn sanitize_url_string_removes_query_and_fragment() {
        let raw = "https://example.com/path?token=secret#frag";
        assert_eq!(sanitize_url_string(raw), "https://example.com/path");
    }
}
