use crate::engine::VpnError;
use std::net::{Ipv4Addr, SocketAddrV4};
use std::time::{Duration, Instant};
use tokio::net::{lookup_host, TcpStream};
use windows::Win32::NetworkManagement::IpHelper::{
    IcmpCreateFile, IcmpCloseHandle, IcmpSendEcho,
};

pub struct DnsResolver;

impl DnsResolver {
    pub fn extract_host_and_optional_ip(raw_input: &str) -> (String, Option<Ipv4Addr>) {
        let mut trimmed = raw_input.trim();
        trimmed = trimmed.trim_matches(|c| c == '"' || c == '\'' || c == '<' || c == '>' || c == '`');

        let tokens: Vec<&str> = trimmed.split_whitespace().collect();
        if tokens.len() == 2 {
            let host = Self::extract_host(tokens[0]);
            let ip = tokens[1].parse::<Ipv4Addr>().ok();
            return (host, ip);
        }

        if let Some((h, ip_str)) = trimmed.split_once('=') {
            if let Ok(ip) = ip_str.trim().parse::<Ipv4Addr>() {
                let host = Self::extract_host(h);
                return (host, Some(ip));
            }
        }

        (Self::extract_host(trimmed), None)
    }

    pub fn extract_host(raw_input: &str) -> String {
        let mut trimmed = raw_input.trim();
        trimmed = trimmed.trim_matches(|c| c == '"' || c == '\'' || c == '<' || c == '>' || c == '`');

        for part in trimmed.split(';') {
            let p = part.trim();
            if let Some((k, v)) = p.split_once('=') {
                let key = k.trim().to_lowercase();
                if key == "server" || key == "host" || key == "data source" || key == "hostname" {
                    return Self::extract_host(v);
                }
            }
        }

        if let Some(idx) = trimmed.rfind("://") {
            trimmed = &trimmed[idx + 3..];
        }

        if let Some(idx) = trimmed.rfind('@') {
            trimmed = &trimmed[idx + 1..];
        }

        let host_and_port = trimmed.split('/').next().unwrap_or(trimmed);
        let host_clean_query = host_and_port.split('?').next().unwrap_or(host_and_port);
        let host_no_port = host_clean_query.split(':').next().unwrap_or(host_clean_query);
        host_no_port.trim().to_lowercase()
    }

    pub async fn resolve_ipv4(host: &str) -> Result<Ipv4Addr, VpnError> {
        let (clean_host, manual_ip) = Self::extract_host_and_optional_ip(host);
        if let Some(ip) = manual_ip {
            return Ok(ip);
        }

        if let Ok(ip) = clean_host.parse::<Ipv4Addr>() {
            return Ok(ip);
        }

        let query_0 = format!("{clean_host}:0");
        if let Ok(addrs) = lookup_host(&query_0).await {
            for addr in addrs {
                if let std::net::SocketAddr::V4(v4) = addr {
                    return Ok(*v4.ip());
                }
            }
        }

        let query = format!("{clean_host}:80");
        if let Ok(addrs) = lookup_host(&query).await {
            for addr in addrs {
                if let std::net::SocketAddr::V4(v4) = addr {
                    return Ok(*v4.ip());
                }
            }
        }

        let query_db = format!("{clean_host}:3306");
        if let Ok(addrs) = lookup_host(&query_db).await {
            for addr in addrs {
                if let std::net::SocketAddr::V4(v4) = addr {
                    return Ok(*v4.ip());
                }
            }
        }

        Err(VpnError::Dns(format!(
            "Could not resolve IPv4 for '{clean_host}'. If this is a private internal database host, specify IP explicitly: '{clean_host} <IP>'"
        )))
    }

    pub async fn ping_target(target: &str) -> Result<u32, VpnError> {
        let host = Self::extract_host(target);
        let ip = if let Ok(ip) = host.parse::<Ipv4Addr>() {
            ip
        } else {
            Self::resolve_ipv4(&host).await?
        };

        if let Ok(ms) = Self::probe_tcp_latency(ip, 3306, Duration::from_millis(1500)).await {
            return Ok(ms);
        }
        if let Ok(ms) = Self::probe_tcp_latency(ip, 443, Duration::from_millis(1500)).await {
            return Ok(ms);
        }
        if let Ok(ms) = Self::probe_tcp_latency(ip, 80, Duration::from_millis(1500)).await {
            return Ok(ms);
        }
        Self::probe_icmp_latency(ip, 1500)
    }

    pub async fn probe_tcp_latency(ip: Ipv4Addr, port: u16, timeout_dur: Duration) -> Result<u32, VpnError> {
        let target = SocketAddrV4::new(ip, port);
        let start = Instant::now();

        tokio::time::timeout(timeout_dur, TcpStream::connect(target))
            .await
            .map_err(|_| VpnError::Dns("Connection probe timed out".into()))?
            .map_err(|e| VpnError::Dns(format!("TCP handshake failed: {e}")))?;

        Ok(start.elapsed().as_millis() as u32)
    }

    pub fn probe_icmp_latency(ip: Ipv4Addr, timeout_ms: u32) -> Result<u32, VpnError> {
        // SAFETY: Instantiating Win32 ICMP handle for non-privileged echo request execution.
        let handle = unsafe { IcmpCreateFile() }
            .map_err(|e| VpnError::Win32(e.code().0 as u32, "IcmpCreateFile failed".into()))?;

        let send_data = [0xAAu8; 32];
        let reply_size = std::mem::size_of::<windows::Win32::NetworkManagement::IpHelper::ICMP_ECHO_REPLY>() + 32 + 8;
        let mut reply_buffer = vec![0u8; reply_size];
        let ip_u32 = u32::from_ne_bytes(ip.octets());

        // SAFETY: Dispatching ICMP Echo request to Windows IP Helper stack with allocated reply buffer.
        let replies = unsafe {
            IcmpSendEcho(
                handle,
                ip_u32,
                send_data.as_ptr() as *const _,
                send_data.len() as u16,
                None,
                reply_buffer.as_mut_ptr() as *mut _,
                reply_size as u32,
                timeout_ms,
            )
        };

        // SAFETY: Explicitly closing ICMP file handle to prevent kernel resource leakage.
        unsafe { let _ = IcmpCloseHandle(handle); };

        if replies == 0 {
            return Err(VpnError::Dns("ICMP echo request failed or timed out".into()));
        }

        let echo_reply = unsafe {
            &*(reply_buffer.as_ptr() as *const windows::Win32::NetworkManagement::IpHelper::ICMP_ECHO_REPLY)
        };

        Ok(echo_reply.RoundTripTime)
    }
}
