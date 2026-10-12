use crate::dns::DnsResolver;
use crate::dns_proxy::DnsProxy;
use crate::engine::{ConnectionState, VpnEngine, VpnStatus};
use crate::hosts_manager::HostsManager;
use crate::openvpn::OpenVpnEngine;
use crate::routing::RoutingManager;
use crate::service_ipc::{ServiceRequest, ServiceResponse, PIPE_NAME};

use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::windows::named_pipe::{ClientOptions, ServerOptions};
use tokio::sync::RwLock;

pub struct CoreDaemon {
    engine: Arc<OpenVpnEngine>,
    hosts: HostsManager,
    routing: Arc<RwLock<Option<RoutingManager>>>,
    managed_hosts: Arc<RwLock<HashMap<String, Option<Ipv4Addr>>>>,
    dns_proxy: Arc<DnsProxy>,
}

impl CoreDaemon {
    pub fn new() -> Self {
        let upstream = SocketAddr::from(([1, 1, 1, 1], 53));
        Self {
            engine: Arc::new(OpenVpnEngine::new(11940)),
            hosts: HostsManager::new(),
            routing: Arc::new(RwLock::new(None)),
            managed_hosts: Arc::new(RwLock::new(HashMap::new())),
            dns_proxy: Arc::new(DnsProxy::new(upstream)),
        }
    }

    pub async fn handle_request(&self, req: ServiceRequest) -> ServiceResponse {
        match req {
            ServiceRequest::StartVpn {
                config_content,
                selective_routing,
                extra_routes,
            } => {
                let sanitized = OpenVpnEngine::sanitize_config(&config_content, selective_routing, &extra_routes);
                let temp_dir = std::env::temp_dir();
                let config_path = temp_dir.join("joyful_active.ovpn");
                if let Err(e) = std::fs::write(&config_path, sanitized.as_bytes()) {
                    return ServiceResponse::Error(e.to_string());
                }

                self.engine.add_log(format!("[SERVICE] Written config to {}", config_path.display())).await;
                if let Err(e) = self.engine.start(&config_path).await {
                    return ServiceResponse::Error(e.to_string());
                }

                let _ = self.dns_proxy.start(5353).await;

                let state_engine = Arc::clone(&self.engine);
                let state_routing = Arc::clone(&self.routing);
                let state_hosts = Arc::clone(&self.managed_hosts);
                let state_dns = Arc::clone(&self.dns_proxy);

                tokio::spawn(async move {
                    for _ in 0..60 {
                        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
                        if let Ok(st) = state_engine.get_status().await {
                            if st.state == ConnectionState::Connected {
                                state_engine.add_log("[SERVICE] Connection established. Detecting TUN interface...".into()).await;
                                let local_ip = st.local_ip.as_ref().and_then(|g| g.parse().ok());
                                let gw = st.gateway_ip.as_ref().and_then(|g| g.parse().ok());
                                if let Ok(rm) = RoutingManager::detect_vpn_interface(local_ip, gw) {
                                    let mut hosts = state_hosts.write().await;
                                    for (h, ip_opt) in hosts.iter_mut() {
                                        let ip = if let Some(ip) = *ip_opt {
                                            ip
                                        } else if let Ok(res) = DnsResolver::resolve_ipv4(h).await {
                                            *ip_opt = Some(res);
                                            res
                                        } else {
                                            continue;
                                        };
                                        let _ = rm.add_host_route(ip);
                                        state_dns.update_host(h.clone(), ip).await;
                                        state_engine.add_log(format!("[SERVICE-ROUTE] {h} -> {ip}")).await;
                                    }
                                    *state_routing.write().await = Some(rm);
                                    state_engine.add_log("[SERVICE] Live routing synced with kernel & DNS proxy.".into()).await;
                                }
                                break;
                            }
                        }
                    }
                });

                ServiceResponse::Ok
            }
            ServiceRequest::StopVpn => {
                *self.routing.write().await = None;
                self.dns_proxy.stop().await;
                let _ = self.hosts.revert_to_backup().await;
                match self.engine.stop().await {
                    Ok(_) => ServiceResponse::Ok,
                    Err(e) => ServiceResponse::Error(e.to_string()),
                }
            }
            ServiceRequest::GetStatus => {
                match self.engine.get_status().await {
                    Ok(st) => ServiceResponse::Status(st),
                    Err(e) => ServiceResponse::Error(e.to_string()),
                }
            }
            ServiceRequest::GetLogs => {
                let logs = self.engine.get_logs().await;
                ServiceResponse::Logs(logs)
            }
            ServiceRequest::AddHost { host, ip } => {
                let (clean_host, manual_ip) = DnsResolver::extract_host_and_optional_ip(&host);
                if clean_host.is_empty() {
                    return ServiceResponse::Error("Invalid hostname".into());
                }

                let target_ip = if let Some(i) = ip.or(manual_ip) {
                    Some(i)
                } else {
                    DnsResolver::resolve_ipv4(&clean_host).await.ok()
                };

                let ip_str = if let Some(valid_ip) = target_ip {
                    {
                        let mut map = self.managed_hosts.write().await;
                        map.insert(clean_host.clone(), Some(valid_ip));
                        let pairs: Vec<(Ipv4Addr, String)> = map.iter().filter_map(|(h, ip)| ip.map(|i| (i, h.clone()))).collect();
                        let _ = self.hosts.apply_entries(&pairs).await;
                    }

                    self.dns_proxy.update_host(clean_host.clone(), valid_ip).await;

                    if let Some(ref rm) = *self.routing.read().await {
                        let _ = rm.add_host_route(valid_ip);
                        self.engine.add_log(format!("[SERVICE-HOTROUTE] {clean_host} -> {valid_ip}")).await;
                    }
                    valid_ip.to_string()
                } else {
                    {
                        let mut map = self.managed_hosts.write().await;
                        map.insert(clean_host.clone(), None);
                    }
                    "Pending VPN".to_string()
                };

                ServiceResponse::HostAdded {
                    host: clean_host,
                    ip: ip_str,
                }
            }
            ServiceRequest::RemoveHost { host, ip } => {
                let (clean_host, manual_ip) = DnsResolver::extract_host_and_optional_ip(&host);
                let target_ip = ip.or(manual_ip);
                {
                    let mut map = self.managed_hosts.write().await;
                    map.remove(&clean_host);
                    let pairs: Vec<(Ipv4Addr, String)> = map.iter().filter_map(|(h, ip)| ip.map(|i| (i, h.clone()))).collect();
                    let _ = self.hosts.apply_entries(&pairs).await;
                }
                self.dns_proxy.remove_host(&clean_host).await;

                if let Some(i) = target_ip {
                    if let Some(ref rm) = *self.routing.read().await {
                        let _ = rm.remove_host_route(i);
                    }
                }
                ServiceResponse::Ok
            }
            ServiceRequest::Ping { target } => {
                match DnsResolver::ping_target(&target).await {
                    Ok(rtt) => ServiceResponse::PingResult(rtt),
                    Err(e) => ServiceResponse::Error(e.to_string()),
                }
            }
        }
    }

    pub async fn run_ipc_server(self: Arc<Self>) -> Result<(), Box<dyn std::error::Error>> {
        loop {
            let server = match ServerOptions::new().first_pipe_instance(true).create(PIPE_NAME) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("Failed to bind pipe {PIPE_NAME}: {e}");
                    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
                    continue;
                }
            };

            if server.connect().await.is_err() {
                continue;
            }

            let daemon = Arc::clone(&self);
            tokio::spawn(async move {
                let (reader, mut writer) = tokio::io::split(server);
                let mut lines = BufReader::new(reader).lines();

                while let Ok(Some(line)) = lines.next_line().await {
                    let req: ServiceRequest = match serde_json::from_str(&line) {
                        Ok(r) => r,
                        Err(e) => {
                            let resp = ServiceResponse::Error(format!("Malformed IPC: {e}"));
                            let mut out = serde_json::to_string(&resp).unwrap();
                            out.push('\n');
                            let _ = writer.write_all(out.as_bytes()).await;
                            continue;
                        }
                    };

                    let resp = daemon.handle_request(req).await;
                    let mut out = serde_json::to_string(&resp).unwrap();
                    out.push('\n');
                    if writer.write_all(out.as_bytes()).await.is_err() {
                        break;
                    }
                }
            });
        }
    }
}

pub struct IpcClient;

impl IpcClient {
    pub async fn send_request(req: ServiceRequest) -> Result<ServiceResponse, String> {
        let client = ClientOptions::new().open(PIPE_NAME).map_err(|e| e.to_string())?;
        let (reader, mut writer) = tokio::io::split(client);
        let mut lines = BufReader::new(reader).lines();

        let mut payload = serde_json::to_string(&req).map_err(|e| e.to_string())?;
        payload.push('\n');

        writer.write_all(payload.as_bytes()).await.map_err(|e| e.to_string())?;

        if let Ok(Some(resp_line)) = lines.next_line().await {
            serde_json::from_str(&resp_line).map_err(|e| e.to_string())
        } else {
            Err("Failed to read response from Daemon Pipe".into())
        }
    }
}
