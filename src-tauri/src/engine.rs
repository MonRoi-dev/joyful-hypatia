use std::path::Path;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum VpnError {
    #[error("Wintun adapter error: {0}")]
    Adapter(String),
    #[error("Routing error: {0}")]
    Routing(String),
    #[error("Process error: {0}")]
    Process(String),
    #[error("Management IPC error: {0}")]
    Ipc(String),
    #[error("Hosts manipulation error: {0}")]
    Hosts(String),
    #[error("DNS resolution error: {0}")]
    Dns(String),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Win32 error code {0}: {1}")]
    Win32(u32, String),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ConnectionState {
    Disconnected,
    Connecting,
    Connected,
    Reconnecting,
    Disconnecting,
    Error(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VpnStatus {
    pub state: ConnectionState,
    pub bytes_in: u64,
    pub bytes_out: u64,
    pub connected_duration_sec: u64,
    pub local_ip: Option<String>,
    pub gateway_ip: Option<String>,
}

#[async_trait]
pub trait VpnEngine: Send + Sync {
    async fn start(&self, config_path: &Path) -> Result<(), VpnError>;
    async fn stop(&self) -> Result<(), VpnError>;
    async fn get_status(&self) -> Result<VpnStatus, VpnError>;
    async fn ping(&self) -> Result<u32, VpnError>;
}
