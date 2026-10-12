use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::sync::Arc;
use tokio::net::UdpSocket;
use tokio::sync::RwLock;

pub struct DnsProxy {
    routes: Arc<RwLock<HashMap<String, Ipv4Addr>>>,
    upstream_dns: SocketAddr,
    is_running: Arc<RwLock<bool>>,
}

impl DnsProxy {
    pub fn new(upstream_dns: SocketAddr) -> Self {
        Self {
            routes: Arc::new(RwLock::new(HashMap::new())),
            upstream_dns,
            is_running: Arc::new(RwLock::new(false)),
        }
    }

    pub async fn update_host(&self, domain: String, ip: Ipv4Addr) {
        let clean = domain.trim().trim_end_matches('.').to_lowercase();
        let mut map = self.routes.write().await;
        map.insert(clean, ip);
    }

    pub async fn remove_host(&self, domain: &str) {
        let clean = domain.trim().trim_end_matches('.').to_lowercase();
        let mut map = self.routes.write().await;
        map.remove(&clean);
    }

    pub async fn clear_hosts(&self) {
        let mut map = self.routes.write().await;
        map.clear();
    }

    pub async fn start(&self, listen_port: u16) -> Result<(), String> {
        let bind_addr = SocketAddrV4::new(Ipv4Addr::new(127, 0, 0, 1), listen_port);
        let socket = match UdpSocket::bind(bind_addr).await {
            Ok(s) => Arc::new(s),
            Err(e) => return Err(format!("DNS proxy bind failed on {bind_addr}: {e}")),
        };

        *self.is_running.write().await = true;
        let routes = Arc::clone(&self.routes);
        let upstream = self.upstream_dns;
        let is_running = Arc::clone(&self.is_running);

        tokio::spawn(async move {
            let mut buf = [0u8; 1500];

            while *is_running.read().await {
                let (len, client_addr) = match socket.recv_from(&mut buf).await {
                    Ok(res) => res,
                    Err(_) => break,
                };

                let query_packet = &buf[..len];
                if len < 12 {
                    continue;
                }

                if let Some((domain, qtype, qclass)) = Self::parse_dns_query(query_packet) {
                    let domain_lower = domain.to_lowercase();
                    let target_ip = {
                        let map = routes.read().await;
                        map.get(&domain_lower).copied()
                    };

                    if let Some(ip) = target_ip {
                        if qtype == 1 && qclass == 1 {
                            let response = Self::build_a_record_response(query_packet, &domain, ip);
                            let _ = socket.send_to(&response, client_addr).await;
                            continue;
                        }
                    }
                }

                let forward_socket = match UdpSocket::bind("0.0.0.0:0").await {
                    Ok(s) => s,
                    Err(_) => continue,
                };

                let _ = forward_socket.send_to(query_packet, upstream).await;
                let mut resp_buf = [0u8; 1500];
                let forward_task = async {
                    if let Ok(rlen) = forward_socket.recv(&mut resp_buf).await {
                        let _ = socket.send_to(&resp_buf[..rlen], client_addr).await;
                    }
                };

                let _ = tokio::time::timeout(std::time::Duration::from_millis(1500), forward_task).await;
            }
        });

        Ok(())
    }

    pub async fn stop(&self) {
        *self.is_running.write().await = false;
    }

    fn parse_dns_query(buf: &[u8]) -> Option<(String, u16, u16)> {
        let qdcount = u16::from_be_bytes([buf[4], buf[5]]);
        if qdcount == 0 {
            return None;
        }

        let mut idx = 12;
        let mut labels = Vec::new();

        while idx < buf.len() {
            let len = buf[idx] as usize;
            if len == 0 {
                idx += 1;
                break;
            }
            idx += 1;
            if idx + len > buf.len() {
                return None;
            }
            let label = std::str::from_utf8(&buf[idx..idx + len]).ok()?;
            labels.push(label);
            idx += len;
        }

        if idx + 4 > buf.len() {
            return None;
        }

        let qtype = u16::from_be_bytes([buf[idx], buf[idx + 1]]);
        let qclass = u16::from_be_bytes([buf[idx + 2], buf[idx + 3]]);

        Some((labels.join("."), qtype, qclass))
    }

    fn build_a_record_response(query: &[u8], _domain: &str, ip: Ipv4Addr) -> Vec<u8> {
        let mut resp = Vec::with_capacity(query.len() + 16);
        resp.extend_from_slice(&query[0..2]);
        resp.push(0x81);
        resp.push(0x80);
        resp.extend_from_slice(&query[4..6]);
        resp.extend_from_slice(&[0x00, 0x01]);
        resp.extend_from_slice(&[0x00, 0x00]);
        resp.extend_from_slice(&[0x00, 0x00]);

        let mut q_end = 12;
        while q_end < query.len() && query[q_end] != 0 {
            q_end += 1 + query[q_end] as usize;
        }
        q_end += 5;

        if q_end <= query.len() {
            resp.extend_from_slice(&query[12..q_end]);
        }

        resp.extend_from_slice(&[0xc0, 0x0c]);
        resp.extend_from_slice(&[0x00, 0x01]);
        resp.extend_from_slice(&[0x00, 0x01]);
        resp.extend_from_slice(&[0x00, 0x00, 0x00, 0x0a]);
        resp.extend_from_slice(&[0x00, 0x04]);
        resp.extend_from_slice(&ip.octets());

        resp
    }
}
