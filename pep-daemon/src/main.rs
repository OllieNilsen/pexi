use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use bytes::Bytes;
use clap::{Parser, Subcommand};
use reqwest::Method;
use reqwest::Url;
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use std::env;
use std::fs;
use std::fs::OpenOptions;
use std::io::{self, Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, TcpListener, ToSocketAddrs};
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use thiserror::Error;
#[cfg(not(target_os = "macos"))]
use vsock::VsockListener;
use vsock::{VMADDR_CID_ANY, VMADDR_CID_HOST, VsockStream};

#[derive(Debug, Parser)]
#[command(name = "avf-vsock-host")]
#[command(about = "AVF vsock + fetch mediation spike tools")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Start a vsock HTTP proxy stub on the host.
    VsockStub {
        #[arg(long, default_value_t = VMADDR_CID_ANY)]
        cid: u32,
        #[arg(long, default_value_t = 4040)]
        port: u32,
        #[arg(long, default_value_t = 10)]
        connect_timeout_secs: u64,
        #[arg(long, default_value_t = 30)]
        request_timeout_secs: u64,
    },
    /// Send a single HTTP request over vsock (for VM-side use).
    VsockClient {
        #[arg(long, default_value_t = VMADDR_CID_HOST)]
        cid: u32,
        #[arg(long, default_value_t = 4040)]
        port: u32,
        #[arg(long)]
        method: Option<String>,
        #[arg(long)]
        url: String,
        #[arg(long)]
        header: Vec<String>,
        #[arg(long)]
        body_file: Option<PathBuf>,
        #[arg(long, default_value_t = false)]
        body_stdin: bool,
    },
    /// Boot a VM by running a Swift AVF helper.
    BootVm {
        #[arg(long)]
        swift_script: PathBuf,
        #[arg(long)]
        kernel: Option<PathBuf>,
        #[arg(long)]
        initrd: Option<PathBuf>,
        #[arg(long)]
        disk: PathBuf,
        #[arg(long)]
        seed: Option<PathBuf>,
        #[arg(long, default_value_t = 2)]
        cpus: u32,
        #[arg(long, default_value_t = 1024 * 1024 * 1024)]
        memory_bytes: u64,
        #[arg(long, default_value_t = 4040)]
        vsock_port: u32,
        #[arg(long, default_value_t = 4041)]
        bridge_port: u16,
        #[arg(long)]
        cmdline: Option<String>,
        #[arg(long)]
        console_log: Option<PathBuf>,
        #[arg(long)]
        status_log: Option<PathBuf>,
        #[arg(long, default_value_t = false)]
        efi: bool,
        #[arg(long)]
        efi_vars: Option<PathBuf>,
        #[arg(long)]
        shared_dir: Option<PathBuf>,
    },
}

#[derive(Debug, Serialize, Deserialize)]
struct HttpRequest {
    method: String,
    url: String,
    headers: Vec<(String, String)>,
    body_base64: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct HttpResponse {
    status: u16,
    headers: Vec<(String, String)>,
    body_base64: Option<String>,
    error: Option<ErrorEnvelope>,
}

#[derive(Debug, Serialize, Deserialize)]
struct ErrorEnvelope {
    code: String,
    message: String,
}

#[derive(Debug, Error)]
enum StubError {
    #[error("io error: {0}")]
    Io(#[from] io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),
}

#[derive(Clone, Debug)]
struct StubConfig {
    allowed_domains: Vec<String>,
    max_request_bytes: usize,
    max_response_bytes: usize,
    max_redirects: u32,
    audit_log_path: PathBuf,
}

impl StubConfig {
    fn from_env() -> Self {
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
            .unwrap_or_else(|_| PathBuf::from("spikes/vm-node-fetch/audit.jsonl"));

        Self {
            allowed_domains,
            max_request_bytes,
            max_response_bytes,
            max_redirects,
            audit_log_path,
        }
    }
}

#[derive(Debug, Serialize)]
struct AuditEntry {
    ts_unix_ms: u64,
    method: String,
    url: String,
    status: u16,
    error_code: Option<String>,
    request_bytes: usize,
    response_bytes: usize,
    redirects: u32,
    decision: String,
}

fn main() -> Result<(), StubError> {
    let cli = Cli::parse();

    match cli.command {
        Commands::VsockStub {
            cid,
            port,
            connect_timeout_secs,
            request_timeout_secs,
        } => run_stub(cid, port, connect_timeout_secs, request_timeout_secs),
        Commands::VsockClient {
            cid,
            port,
            method,
            url,
            header,
            body_file,
            body_stdin,
        } => run_client(cid, port, method, url, header, body_file, body_stdin),
        Commands::BootVm {
            swift_script,
            kernel,
            initrd,
            disk,
            seed,
            cpus,
            memory_bytes,
            vsock_port,
            bridge_port,
            cmdline,
            console_log,
            status_log,
            efi,
            efi_vars,
            shared_dir,
        } => run_boot_vm(
            swift_script,
            kernel,
            initrd,
            disk,
            seed,
            cpus,
            memory_bytes,
            vsock_port,
            bridge_port,
            cmdline,
            console_log,
            status_log,
            efi,
            efi_vars,
            shared_dir,
        ),
    }
}

fn run_stub(
    _cid: u32,
    port: u32,
    connect_timeout_secs: u64,
    request_timeout_secs: u64,
) -> Result<(), StubError> {
    let client = Client::builder()
        .connect_timeout(Duration::from_secs(connect_timeout_secs))
        .timeout(Duration::from_secs(request_timeout_secs))
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let config = StubConfig::from_env();

    #[cfg(target_os = "macos")]
    {
        let addr = format!("127.0.0.1:{port}");
        let listener = TcpListener::bind(&addr)?;
        eprintln!("tcp stub listening on {addr} (macOS; vsock forwarded by AVF)");
        for conn in listener.incoming() {
            let mut stream = conn?;
            if let Err(err) = handle_connection(&mut stream, &client, &config) {
                eprintln!("connection error: {err}");
            }
        }
        Ok(())
    }

    #[cfg(not(target_os = "macos"))]
    {
        let listener = VsockListener::bind_with_cid_port(_cid, port)?;
        eprintln!("vsock stub listening on cid={_cid} port={port}");
        for conn in listener.incoming() {
            let mut stream = conn?;
            if let Err(err) = handle_connection(&mut stream, &client, &config) {
                eprintln!("connection error: {err}");
            }
        }
        Ok(())
    }
}

fn handle_connection<S: Read + Write>(
    stream: &mut S,
    client: &Client,
    config: &StubConfig,
) -> Result<(), StubError> {
    loop {
        let request_frame = match read_frame(stream) {
            Ok(frame) => frame,
            Err(err) if err.kind() == io::ErrorKind::UnexpectedEof => return Ok(()),
            Err(err) => return Err(StubError::Io(err)),
        };
        let request: HttpRequest = serde_json::from_slice(&request_frame)?;
        let response = execute_request(client, request, config)?;
        let response_bytes = serde_json::to_vec(&response)?;
        write_frame(stream, &response_bytes)?;
    }
}

fn execute_request(
    client: &Client,
    request: HttpRequest,
    config: &StubConfig,
) -> Result<HttpResponse, StubError> {
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

fn is_scheme_allowed(scheme: &str) -> bool {
    matches!(scheme, "http" | "https")
}

fn is_host_allowed(host: &str, allowlist: &[String]) -> bool {
    if allowlist.is_empty() {
        return false;
    }
    let host = host.trim_end_matches('.').to_lowercase();
    allowlist.iter().any(|entry| {
        let entry = entry.trim_end_matches('.').to_lowercase();
        host == entry || host.ends_with(&format!(".{entry}"))
    })
}

fn ensure_public_host(url: &Url) -> Result<(), String> {
    let host = url.host_str().ok_or_else(|| "missing host".to_string())?;

    if let Ok(ip) = host.parse::<IpAddr>() {
        if !is_public_ip(ip) {
            return Err(format!("blocked ip {ip}"));
        }
        return Ok(());
    }

    let port = url
        .port_or_known_default()
        .ok_or_else(|| "missing port".to_string())?;

    let addrs = (host, port)
        .to_socket_addrs()
        .map_err(|err| format!("dns failed: {err}"))?;

    for addr in addrs {
        let ip = addr.ip();
        if !is_public_ip(ip) {
            return Err(format!("blocked ip {ip}"));
        }
    }

    Ok(())
}

fn is_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(addr) => is_public_ipv4(addr),
        IpAddr::V6(addr) => is_public_ipv6(addr),
    }
}

fn is_public_ipv4(addr: Ipv4Addr) -> bool {
    if addr.is_private()
        || addr.is_loopback()
        || addr.is_link_local()
        || addr.is_multicast()
        || addr.is_broadcast()
        || addr.is_unspecified()
    {
        return false;
    }

    let octets = addr.octets();
    let is_cgnat = octets[0] == 100 && (octets[1] & 0b1100_0000) == 0b0100_0000;
    if is_cgnat {
        return false;
    }

    true
}

fn is_public_ipv6(addr: Ipv6Addr) -> bool {
    if addr.is_loopback()
        || addr.is_unspecified()
        || addr.is_multicast()
        || addr.is_unique_local()
        || addr.is_unicast_link_local()
    {
        return false;
    }
    true
}

fn read_body_with_cap(
    mut response: reqwest::blocking::Response,
    cap: usize,
) -> Result<Vec<u8>, String> {
    read_with_cap(&mut response, cap)
}

fn read_with_cap<R: Read>(reader: &mut R, cap: usize) -> Result<Vec<u8>, String> {
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

fn error_response(code: &str, message: &str) -> HttpResponse {
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

fn sanitize_url(url: &Url) -> String {
    let mut sanitized = url.clone();
    sanitized.set_query(None);
    sanitized.set_fragment(None);
    sanitized.to_string()
}

fn sanitize_url_string(raw: &str) -> String {
    let trimmed = raw.split('#').next().unwrap_or(raw);
    trimmed.split('?').next().unwrap_or(trimmed).to_string()
}

#[allow(clippy::too_many_arguments)]
fn append_audit_entry(
    config: &StubConfig,
    request: &HttpRequest,
    url: String,
    status: u16,
    error_code: Option<&str>,
    request_bytes: usize,
    response_bytes: usize,
    redirects: u32,
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

fn run_client(
    cid: u32,
    port: u32,
    method: Option<String>,
    url: String,
    header: Vec<String>,
    body_file: Option<PathBuf>,
    body_stdin: bool,
) -> Result<(), StubError> {
    let mut headers = Vec::new();
    for entry in header {
        let Some((key, value)) = entry.split_once(':') else {
            continue;
        };
        headers.push((key.trim().to_string(), value.trim().to_string()));
    }
    let body_base64 = if let Some(path) = body_file {
        Some(BASE64.encode(fs::read(path)?))
    } else if body_stdin {
        let mut buf = Vec::new();
        io::stdin().read_to_end(&mut buf)?;
        if buf.is_empty() {
            None
        } else {
            Some(BASE64.encode(buf))
        }
    } else {
        None
    };

    let request = HttpRequest {
        method: method.unwrap_or_else(|| "GET".to_string()),
        url,
        headers,
        body_base64,
    };
    let payload = serde_json::to_vec(&request)?;

    let mut stream = VsockStream::connect_with_cid_port(cid, port)?;
    write_frame(&mut stream, &payload)?;
    let response_bytes = read_frame(&mut stream)?;
    let response: HttpResponse = serde_json::from_slice(&response_bytes)?;
    println!("{}", serde_json::to_string_pretty(&response)?);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn run_boot_vm(
    swift_script: PathBuf,
    kernel: Option<PathBuf>,
    initrd: Option<PathBuf>,
    disk: PathBuf,
    seed: Option<PathBuf>,
    cpus: u32,
    memory_bytes: u64,
    vsock_port: u32,
    bridge_port: u16,
    cmdline: Option<String>,
    console_log: Option<PathBuf>,
    status_log: Option<PathBuf>,
    efi: bool,
    efi_vars: Option<PathBuf>,
    shared_dir: Option<PathBuf>,
) -> Result<(), StubError> {
    if !swift_script.exists() {
        return Err(StubError::Io(io::Error::new(
            io::ErrorKind::NotFound,
            format!("swift script not found: {}", swift_script.display()),
        )));
    }
    if !disk.exists() {
        return Err(StubError::Io(io::Error::new(
            io::ErrorKind::NotFound,
            format!("disk not found: {}", disk.display()),
        )));
    }
    if !efi {
        let kernel = kernel.as_ref().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "kernel is required unless --efi",
            )
        })?;
        let initrd = initrd.as_ref().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "initrd is required unless --efi",
            )
        })?;
        if !kernel.exists() {
            return Err(StubError::Io(io::Error::new(
                io::ErrorKind::NotFound,
                format!("kernel not found: {}", kernel.display()),
            )));
        }
        if !initrd.exists() {
            return Err(StubError::Io(io::Error::new(
                io::ErrorKind::NotFound,
                format!("initrd not found: {}", initrd.display()),
            )));
        }
    }
    if let Some(dir) = &shared_dir
        && !dir.exists()
    {
        return Err(StubError::Io(io::Error::new(
            io::ErrorKind::NotFound,
            format!("shared dir not found: {}", dir.display()),
        )));
    }
    if let Some(seed) = &seed
        && !seed.exists()
    {
        return Err(StubError::Io(io::Error::new(
            io::ErrorKind::NotFound,
            format!("seed image not found: {}", seed.display()),
        )));
    }

    if swift_script
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext == "swift")
        .unwrap_or(false)
    {
        return Err(StubError::Io(io::Error::new(
            io::ErrorKind::InvalidInput,
            "swift runner must be a compiled binary, not a .swift script",
        )));
    }
    let mut cmd = Command::new(&swift_script);
    if let Some(kernel) = kernel {
        cmd.arg("--kernel").arg(kernel);
    }
    if let Some(initrd) = initrd {
        cmd.arg("--initrd").arg(initrd);
    }
    cmd.arg("--disk")
        .arg(disk)
        .arg("--cpus")
        .arg(cpus.to_string())
        .arg("--memory-bytes")
        .arg(memory_bytes.to_string())
        .arg("--vsock-port")
        .arg(vsock_port.to_string());
    if let Some(seed) = seed {
        cmd.arg("--seed").arg(seed);
    }
    cmd.arg("--bridge-port").arg(bridge_port.to_string());
    if let Some(cmdline) = cmdline {
        cmd.arg("--cmdline").arg(cmdline);
    }
    if let Some(console_log) = console_log {
        cmd.arg("--console-log").arg(console_log);
    }
    if let Some(status_log) = status_log {
        cmd.arg("--status-log").arg(status_log);
    }
    if efi {
        cmd.arg("--efi");
    }
    if let Some(efi_vars) = efi_vars {
        cmd.arg("--efi-vars").arg(efi_vars);
    }
    if let Some(shared_dir) = shared_dir {
        cmd.arg("--shared-dir").arg(shared_dir);
    }
    let status = cmd.status()?;
    if !status.success() {
        return Err(StubError::Io(io::Error::other(format!(
            "swift runner exited with {status}"
        ))));
    }
    Ok(())
}

fn read_frame<R: Read>(stream: &mut R) -> io::Result<Vec<u8>> {
    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf)?;
    let len = u32::from_be_bytes(len_buf) as usize;
    let mut buf = vec![0u8; len];
    stream.read_exact(&mut buf)?;
    Ok(buf)
}

fn write_frame<W: Write>(stream: &mut W, data: &[u8]) -> io::Result<()> {
    let len = data.len() as u32;
    stream.write_all(&len.to_be_bytes())?;
    stream.write_all(data)?;
    stream.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::net::IpAddr;

    #[test]
    fn host_allowlist_accepts_exact_and_subdomain() {
        let allowlist = vec!["example.com".to_string()];
        assert!(is_host_allowed("example.com", &allowlist));
        assert!(is_host_allowed("api.example.com", &allowlist));
        assert!(!is_host_allowed("evil-example.com", &allowlist));
        assert!(!is_host_allowed("example.com.evil", &allowlist));
    }

    #[test]
    fn host_allowlist_is_case_insensitive() {
        let allowlist = vec!["Example.COM".to_string()];
        assert!(is_host_allowed("API.Example.Com", &allowlist));
    }

    #[test]
    fn public_ipv4_blocks_private_ranges() {
        let private_ips = [
            "10.0.0.1",
            "192.168.1.1",
            "127.0.0.1",
            "169.254.1.1",
            "100.64.0.1",
        ];
        for ip in private_ips {
            let addr: IpAddr = ip.parse().unwrap();
            assert!(!is_public_ip(addr), "expected {ip} to be blocked");
        }
        let public: IpAddr = "8.8.8.8".parse().unwrap();
        assert!(is_public_ip(public));
    }

    #[test]
    fn public_ipv6_blocks_private_ranges() {
        let private_ips = ["::1", "fe80::1", "fc00::1"];
        for ip in private_ips {
            let addr: IpAddr = ip.parse().unwrap();
            assert!(!is_public_ip(addr), "expected {ip} to be blocked");
        }
        let public: IpAddr = "2001:4860:4860::8888".parse().unwrap();
        assert!(is_public_ip(public));
    }

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
