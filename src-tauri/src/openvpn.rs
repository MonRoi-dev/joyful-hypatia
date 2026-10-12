use crate::engine::{ConnectionState, VpnEngine, VpnError, VpnStatus};
use async_trait::async_trait;
use regex::Regex;
use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::process::{Child, Command};
use tokio::sync::RwLock;

pub struct OpenVpnEngine {
    child: Arc<RwLock<Option<Child>>>,
    status: Arc<RwLock<VpnStatus>>,
    is_running: Arc<AtomicBool>,
    started_at: Arc<RwLock<Option<Instant>>>,
    logs: Arc<RwLock<Vec<String>>>,
    mgmt_port: u16,
}

impl OpenVpnEngine {
    pub fn new(mgmt_port: u16) -> Self {
        Self {
            child: Arc::new(RwLock::new(None)),
            status: Arc::new(RwLock::new(VpnStatus {
                state: ConnectionState::Disconnected,
                bytes_in: 0,
                bytes_out: 0,
                connected_duration_sec: 0,
                local_ip: None,
                gateway_ip: None,
            })),
            is_running: Arc::new(AtomicBool::new(false)),
            started_at: Arc::new(RwLock::new(None)),
            logs: Arc::new(RwLock::new(Vec::new())),
            mgmt_port,
        }
    }

    pub fn resolve_openvpn_binary() -> Result<PathBuf, VpnError> {
        let mut candidates = vec![
            PathBuf::from(r"C:\Program Files\OpenVPN\bin\openvpn.exe"),
            PathBuf::from(r"C:\Program Files (x86)\OpenVPN\bin\openvpn.exe"),
            PathBuf::from(r"C:\Program Files\AmneziaVPN\openvpn.exe"),
        ];

        if let Ok(path_var) = std::env::var("PATH") {
            for dir in path_var.split(';') {
                let p = Path::new(dir).join("openvpn.exe");
                if p.exists() {
                    candidates.push(p);
                }
            }
        }

        if let Ok(exe_path) = std::env::current_exe() {
            if let Some(parent) = exe_path.parent() {
                candidates.push(parent.join("openvpn.exe"));
                candidates.push(parent.join("bin").join("openvpn.exe"));
            }
        }

        for path in &candidates {
            if path.exists() {
                return Ok(path.clone());
            }
        }

        Err(VpnError::Process(
            "openvpn.exe not found. Install OpenVPN (C:\\Program Files\\OpenVPN\\bin) or place openvpn.exe with dependent DLLs next to joyful-hypatia.exe".into()
        ))
    }

    pub fn sanitize_config(raw_content: &str, selective_routing: bool, extra_routes: &[Ipv4Addr]) -> String {
        let mut sanitized = String::new();
        let mut has_wintun_driver = false;
        let mut has_dev = false;

        for line in raw_content.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with("windows-driver") {
                has_wintun_driver = true;
            }
            if trimmed.starts_with("dev ") || trimmed == "dev" {
                has_dev = true;
            }
            if selective_routing && (trimmed.starts_with("redirect-gateway") || trimmed.contains("redirect-gateway")) {
                continue;
            }
            if selective_routing && trimmed.starts_with("route ") {
                continue;
            }
            sanitized.push_str(line);
            sanitized.push('\n');
        }

        if !has_dev {
            sanitized.push_str("\ndev tun\n");
        }

        if !has_wintun_driver {
            sanitized.push_str("windows-driver wintun\n");
        }

        if selective_routing {
            sanitized.push_str("route-nopull\n");
            sanitized.push_str("pull-filter ignore \"redirect-gateway\"\n");
            for ip in extra_routes {
                sanitized.push_str(&format!("route {ip} 255.255.255.255 vpn_gateway\n"));
            }
        }

        sanitized
    }

    pub async fn get_logs(&self) -> Vec<String> {
        self.logs.read().await.clone()
    }

    pub async fn add_log(&self, msg: String) {
        let mut l = self.logs.write().await;
        if l.len() > 500 {
            l.remove(0);
        }
        l.push(msg);
    }

    async fn monitor_management_socket(
        port: u16,
        status: Arc<RwLock<VpnStatus>>,
        is_running: Arc<AtomicBool>,
        logs: Arc<RwLock<Vec<String>>>,
    ) {
        let mut attempts = 0;
        let stream = loop {
            if !is_running.load(Ordering::Relaxed) {
                return;
            }
            if attempts > 30 {
                let mut st = status.write().await;
                if st.state != ConnectionState::Connected {
                    st.state = ConnectionState::Error("Failed to connect to OpenVPN management socket".into());
                }
                return;
            }
            if let Ok(s) = TcpStream::connect(format!("127.0.0.1:{port}")).await {
                break s;
            }
            attempts += 1;
            tokio::time::sleep(Duration::from_millis(250)).await;
        };

        let (reader, mut writer) = stream.into_split();
        let mut lines = BufReader::new(reader).lines();

        let _ = writer.write_all(b"bytecount 1\r\nstate on\r\nhold release\r\n").await;

        let bytecount_regex = Regex::new(r">BYTECOUNT:(\d+),(\d+)").unwrap();

        let writer_arc = Arc::new(tokio::sync::Mutex::new(writer));
        let writer_poll = Arc::clone(&writer_arc);
        let is_running_poll = Arc::clone(&is_running);

        tokio::spawn(async move {
            while is_running_poll.load(Ordering::Relaxed) {
                tokio::time::sleep(Duration::from_secs(3)).await;
                let mut w = writer_poll.lock().await;
                let _ = w.write_all(b"state\r\n").await;
            }
        });

        while is_running.load(Ordering::Relaxed) {
            match lines.next_line().await {
                Ok(Some(line)) => {
                    let trimmed = line.trim();

                    if let Some(caps) = bytecount_regex.captures(trimmed) {
                        let bytes_in: u64 = caps[1].parse().unwrap_or(0);
                        let bytes_out: u64 = caps[2].parse().unwrap_or(0);
                        let mut st = status.write().await;
                        st.bytes_in = bytes_in;
                        st.bytes_out = bytes_out;
                    } else if trimmed.starts_with(">STATE:") || (trimmed.contains(',') && (trimmed.contains("CONNECTED") || trimmed.contains("WAIT") || trimmed.contains("AUTH"))) {
                        let content = if let Some(stripped) = trimmed.strip_prefix(">STATE:") {
                            stripped
                        } else {
                            trimmed
                        };

                        let parts: Vec<&str> = content.split(',').collect();
                        if parts.len() >= 2 {
                            let state_name = parts[1];
                            let mut st = status.write().await;
                            match state_name {
                                "CONNECTED" => {
                                    st.state = ConnectionState::Connected;
                                    if parts.len() > 3 && !parts[3].is_empty() {
                                        st.local_ip = Some(parts[3].to_string());
                                    }
                                }
                                "CONNECTING" | "WAIT" | "AUTH" | "GET_CONFIG" | "ASSIGN_IP" => {
                                    if st.state != ConnectionState::Connected {
                                        st.state = ConnectionState::Connecting;
                                    }
                                }
                                "RECONNECTING" => {
                                    st.state = ConnectionState::Reconnecting;
                                }
                                "EXITING" => {
                                    st.state = ConnectionState::Disconnecting;
                                }
                                _ => {}
                            }
                        }
                    } else if !trimmed.starts_with(">INFO") && !trimmed.is_empty() {
                        let mut l = logs.write().await;
                        if l.len() > 500 {
                            l.remove(0);
                        }
                        l.push(format!("[MGMT] {trimmed}"));
                    }
                }
                Ok(None) => break,
                Err(_) => break,
            }
        }
    }
}

#[async_trait]
impl VpnEngine for OpenVpnEngine {
    async fn start(&self, config_path: &Path) -> Result<(), VpnError> {
        let mut kill_cmd = std::process::Command::new("taskkill");
        kill_cmd.args(["/F", "/IM", "openvpn.exe"]);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            kill_cmd.creation_flags(0x08000000);
        }
        let _ = kill_cmd.output();

        let mut child_lock = self.child.write().await;
        if let Some(ref mut child) = *child_lock {
            let _ = child.kill().await;
            *child_lock = None;
        }

        let binary_path = Self::resolve_openvpn_binary()?;

        let mut cmd = Command::new(&binary_path);
        #[cfg(windows)]
        cmd.creation_flags(0x08000000);

        cmd.arg("--config")
            .arg(config_path)
            .arg("--verb")
            .arg("3")
            .arg("--management")
            .arg("127.0.0.1")
            .arg(self.mgmt_port.to_string())
            .arg("--management-hold")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        if let Some(parent) = binary_path.parent() {
            cmd.current_dir(parent);
            let mut path_dirs = vec![parent.to_string_lossy().to_string()];
            if Path::new(r"C:\Program Files\OpenVPN\bin").exists() {
                path_dirs.push(r"C:\Program Files\OpenVPN\bin".into());
            }
            if let Ok(env_path) = std::env::var("PATH") {
                path_dirs.push(env_path);
            }
            cmd.env("PATH", path_dirs.join(";"));
        }

        let mut child = cmd
            .spawn()
            .map_err(|e| VpnError::Process(format!("Failed to spawn {}: {e}", binary_path.display())))?;

        let stdout = child.stdout.take();
        let stderr = child.stderr.take();

        *child_lock = Some(child);
        self.is_running.store(true, Ordering::SeqCst);
        *self.started_at.write().await = Some(Instant::now());

        {
            let mut st = self.status.write().await;
            st.state = ConnectionState::Connecting;
        }

        let status_clone = Arc::clone(&self.status);
        let is_running_clone = Arc::clone(&self.is_running);
        let logs_clone = Arc::clone(&self.logs);
        let port = self.mgmt_port;

        tokio::spawn(async move {
            Self::monitor_management_socket(port, status_clone, is_running_clone, logs_clone).await;
        });

        if let Some(out) = stdout {
            let logs_out = Arc::clone(&self.logs);
            let status_out = Arc::clone(&self.status);
            tokio::spawn(async move {
                let mut lines = BufReader::new(out).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    if line.contains("Initialization Sequence Completed") {
                        let mut st = status_out.write().await;
                        st.state = ConnectionState::Connected;
                    }
                    let mut l = logs_out.write().await;
                    if l.len() > 500 {
                        l.remove(0);
                    }
                    l.push(format!("[VPN] {line}"));
                }
            });
        }

        if let Some(err) = stderr {
            let logs_err = Arc::clone(&self.logs);
            tokio::spawn(async move {
                let mut lines = BufReader::new(err).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    let mut l = logs_err.write().await;
                    if l.len() > 500 {
                        l.remove(0);
                    }
                    l.push(format!("[VPN ERR] {line}"));
                }
            });
        }

        Ok(())
    }

    async fn stop(&self) -> Result<(), VpnError> {
        self.is_running.store(false, Ordering::SeqCst);

        if let Ok(mut stream) = TcpStream::connect(format!("127.0.0.1:{}", self.mgmt_port)).await {
            let _ = stream.write_all(b"signal SIGTERM\r\n").await;
        }

        let mut child_lock = self.child.write().await;
        if let Some(mut child) = child_lock.take() {
            let _ = tokio::time::timeout(Duration::from_millis(500), child.wait()).await;
            let _ = child.kill().await;
        }

        let _ = std::process::Command::new("taskkill").args(["/F", "/IM", "openvpn.exe"]).output();

        let mut st = self.status.write().await;
        st.state = ConnectionState::Disconnected;
        st.bytes_in = 0;
        st.bytes_out = 0;
        st.connected_duration_sec = 0;
        st.local_ip = None;
        st.gateway_ip = None;
        *self.started_at.write().await = None;

        Ok(())
    }

    async fn get_status(&self) -> Result<VpnStatus, VpnError> {
        let mut child_lock = self.child.write().await;
        if let Some(ref mut child) = *child_lock {
            if let Ok(Some(exit_status)) = child.try_wait() {
                *child_lock = None;
                let mut st = self.status.write().await;
                st.state = ConnectionState::Disconnected;
                self.add_log(format!("[INFO] OpenVPN process exited with status: {exit_status}")).await;
            }
        }

        let mut st = self.status.read().await.clone();
        if let Some(started) = *self.started_at.read().await {
            st.connected_duration_sec = started.elapsed().as_secs();
        }
        Ok(st)
    }

    async fn ping(&self) -> Result<u32, VpnError> {
        let st = self.get_status().await?;
        if let Some(gw) = st.gateway_ip {
            if let Ok(ip) = gw.parse() {
                return crate::dns::DnsResolver::probe_icmp_latency(ip, 1000);
            }
        }
        Err(VpnError::Dns("No active gateway for latency probe".into()))
    }
}
