mod audit;
mod config;
mod framing;
mod health;
mod http_exec;
mod policy;
mod ssrf;
mod types;

use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use clap::{Parser, Subcommand};
use std::fs;
use std::io::{self, Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;
#[cfg(not(target_os = "macos"))]
use vsock::VsockListener;
use vsock::{VMADDR_CID_ANY, VMADDR_CID_HOST, VsockStream};

use config::PepConfig;
use framing::{read_frame, write_frame};
use health::health_check;
use http_exec::execute_request;
use policy::{NullEvaluator, PolicyEvaluator, RegorusEvaluator};
use types::{HttpRequest, HttpResponse, PepError};

#[derive(Debug, Parser)]
#[command(name = "pep-daemon")]
#[command(about = "PEP daemon — policy enforcement point for VM sandbox")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Start the PEP daemon (vsock/TCP stub).
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
    /// Check PEP daemon health.
    Health,
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

fn main() -> Result<(), PepError> {
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
        Commands::Health => run_health(),
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

// ── Stub server ──────────────────────────────────────────────────────────

fn build_evaluator(config: &PepConfig) -> Result<Box<dyn PolicyEvaluator>, PepError> {
    if let Some(dir) = &config.policy_dir {
        eprintln!("loading OPA policies from {}", dir.display());
        let eval = RegorusEvaluator::from_dir(dir)?;
        eprintln!("policy hash: {}", eval.policy_hash());
        Ok(Box::new(eval))
    } else {
        eprintln!(
            "no PEP_POLICY_DIR set; using static allowlist ({} domains)",
            config.allowed_domains.len(),
        );
        Ok(Box::new(NullEvaluator::new(config.allowed_domains.clone())))
    }
}

fn run_stub(
    _cid: u32,
    port: u32,
    connect_timeout_secs: u64,
    request_timeout_secs: u64,
) -> Result<(), PepError> {
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(connect_timeout_secs))
        .timeout(Duration::from_secs(request_timeout_secs))
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let config = PepConfig::from_env();
    let evaluator = build_evaluator(&config)?;

    eprintln!(
        "pep-daemon v{} starting (max_response={})",
        env!("CARGO_PKG_VERSION"),
        config.max_response_bytes,
    );

    #[cfg(target_os = "macos")]
    {
        let addr = format!("127.0.0.1:{port}");
        let listener = TcpListener::bind(&addr)?;
        eprintln!("tcp stub listening on {addr} (macOS; vsock forwarded by AVF)");
        for conn in listener.incoming() {
            let mut stream = conn?;
            if let Err(err) = handle_connection(&mut stream, &client, &config, evaluator.as_ref()) {
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
            if let Err(err) = handle_connection(&mut stream, &client, &config, evaluator.as_ref()) {
                eprintln!("connection error: {err}");
            }
        }
        Ok(())
    }
}

fn handle_connection<S: Read + Write>(
    stream: &mut S,
    client: &reqwest::blocking::Client,
    config: &PepConfig,
    evaluator: &dyn PolicyEvaluator,
) -> Result<(), PepError> {
    loop {
        let request_frame = match read_frame(stream) {
            Ok(frame) => frame,
            Err(err) if err.kind() == io::ErrorKind::UnexpectedEof => return Ok(()),
            Err(err) => return Err(PepError::Io(err)),
        };
        let request: HttpRequest = serde_json::from_slice(&request_frame)?;

        // Handle health check requests in-band
        if request.method == "HEALTH" {
            let health = health_check(config);
            let response_bytes = serde_json::to_vec(&health)?;
            write_frame(stream, &response_bytes)?;
            continue;
        }

        let response = execute_request(client, request, config, evaluator)?;
        let response_bytes = serde_json::to_vec(&response)?;
        write_frame(stream, &response_bytes)?;
    }
}

// ── Health check ─────────────────────────────────────────────────────────

fn run_health() -> Result<(), PepError> {
    let config = PepConfig::from_env();
    let health = health_check(&config);
    println!("{}", serde_json::to_string_pretty(&health)?);
    Ok(())
}

// ── Vsock client ─────────────────────────────────────────────────────────

fn run_client(
    cid: u32,
    port: u32,
    method: Option<String>,
    url: String,
    header: Vec<String>,
    body_file: Option<PathBuf>,
    body_stdin: bool,
) -> Result<(), PepError> {
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

// ── Boot VM ──────────────────────────────────────────────────────────────

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
) -> Result<(), PepError> {
    if !swift_script.exists() {
        return Err(PepError::Io(io::Error::new(
            io::ErrorKind::NotFound,
            format!("swift script not found: {}", swift_script.display()),
        )));
    }
    if !disk.exists() {
        return Err(PepError::Io(io::Error::new(
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
            return Err(PepError::Io(io::Error::new(
                io::ErrorKind::NotFound,
                format!("kernel not found: {}", kernel.display()),
            )));
        }
        if !initrd.exists() {
            return Err(PepError::Io(io::Error::new(
                io::ErrorKind::NotFound,
                format!("initrd not found: {}", initrd.display()),
            )));
        }
    }
    if let Some(dir) = &shared_dir
        && !dir.exists()
    {
        return Err(PepError::Io(io::Error::new(
            io::ErrorKind::NotFound,
            format!("shared dir not found: {}", dir.display()),
        )));
    }
    if let Some(seed) = &seed
        && !seed.exists()
    {
        return Err(PepError::Io(io::Error::new(
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
        return Err(PepError::Io(io::Error::new(
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
        return Err(PepError::Io(io::Error::other(format!(
            "swift runner exited with {status}"
        ))));
    }
    Ok(())
}
