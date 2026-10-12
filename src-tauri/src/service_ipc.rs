use crate::engine::VpnStatus;
use serde::{Deserialize, Serialize};
use std::net::Ipv4Addr;

pub const PIPE_NAME: &str = r"\\.\pipe\joyful_vpn_service";

#[derive(Debug, Serialize, Deserialize)]
pub enum ServiceRequest {
    StartVpn {
        config_content: String,
        selective_routing: bool,
        extra_routes: Vec<Ipv4Addr>,
    },
    StopVpn,
    GetStatus,
    GetLogs,
    AddHost {
        host: String,
        ip: Option<Ipv4Addr>,
    },
    RemoveHost {
        host: String,
        ip: Option<Ipv4Addr>,
    },
    Ping {
        target: String,
    },
}

#[derive(Debug, Serialize, Deserialize)]
pub enum ServiceResponse {
    Ok,
    Status(VpnStatus),
    Logs(Vec<String>),
    HostAdded {
        host: String,
        ip: String,
    },
    PingResult(u32),
    Error(String),
}
