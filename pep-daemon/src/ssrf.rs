use reqwest::Url;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, ToSocketAddrs};

pub fn is_scheme_allowed(scheme: &str) -> bool {
    matches!(scheme, "http" | "https")
}

pub fn is_host_allowed(host: &str, allowlist: &[String]) -> bool {
    if allowlist.is_empty() {
        return false;
    }
    let host = host.trim_end_matches('.').to_lowercase();
    allowlist.iter().any(|entry| {
        let entry = entry.trim_end_matches('.').to_lowercase();
        host == entry || host.ends_with(&format!(".{entry}"))
    })
}

pub fn ensure_public_host(url: &Url) -> Result<(), String> {
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

pub fn is_public_ip(ip: IpAddr) -> bool {
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

#[cfg(test)]
mod tests {
    use super::*;
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
}
