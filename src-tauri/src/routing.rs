use crate::engine::VpnError;
use std::net::Ipv4Addr;
use std::process::Command;
use windows::core::{HSTRING, PCWSTR};
use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::NetworkManagement::IpHelper::{
    CreateIpForwardEntry2, DeleteIpForwardEntry2, ConvertInterfaceNameToLuidW,
    ConvertInterfaceLuidToIndex, GetAdaptersAddresses, MIB_IPFORWARD_ROW2,
    IP_ADAPTER_ADDRESSES_LH, GAA_FLAG_INCLUDE_ALL_INTERFACES,
};
use windows::Win32::NetworkManagement::Ndis::NET_LUID_LH;
use windows::Win32::Networking::WinSock::{AF_INET, AF_UNSPEC};

pub struct RoutingManager {
    interface_luid: NET_LUID_LH,
    interface_index: u32,
    adapter_ip: Ipv4Addr,
    gateway_ip: Ipv4Addr,
}

impl RoutingManager {
    pub fn new(interface_name: &str, gateway_ip: Ipv4Addr) -> Result<Self, VpnError> {
        let mut luid = NET_LUID_LH::default();
        let name_hstring = HSTRING::from(interface_name);

        // SAFETY: Converting null-terminated interface wide-string to native LUID representation.
        let luid_res = unsafe {
            ConvertInterfaceNameToLuidW(
                PCWSTR(name_hstring.as_wide().as_ptr()),
                &mut luid,
            )
        };

        if luid_res != ERROR_SUCCESS {
            return Err(VpnError::Routing(format!("ConvertInterfaceNameToLuidW failed: {:?}", luid_res)));
        }

        let mut index: u32 = 0;
        // SAFETY: Resolving interface index from valid LUID structure for IP Helper row dispatch.
        let index_res = unsafe {
            ConvertInterfaceLuidToIndex(&luid, &mut index)
        };

        if index_res != ERROR_SUCCESS {
            return Err(VpnError::Routing(format!("ConvertInterfaceLuidToIndex failed: {:?}", index_res)));
        }

        Ok(Self {
            interface_luid: luid,
            interface_index: index,
            adapter_ip: Ipv4Addr::new(0, 0, 0, 0),
            gateway_ip,
        })
    }

    pub fn detect_vpn_interface(local_ip: Option<Ipv4Addr>, gateway_ip: Option<Ipv4Addr>) -> Result<Self, VpnError> {
        let mut buf_len: u32 = 16384;
        let mut buffer: Vec<u8> = vec![0u8; buf_len as usize];

        // SAFETY: Querying adapter addresses buffer to resolve VPN TUN interface index.
        let ret = unsafe {
            GetAdaptersAddresses(
                AF_UNSPEC.0 as u32,
                GAA_FLAG_INCLUDE_ALL_INTERFACES,
                None,
                Some(buffer.as_mut_ptr() as *mut IP_ADAPTER_ADDRESSES_LH),
                &mut buf_len,
            )
        };

        if ret != ERROR_SUCCESS.0 {
            return Err(VpnError::Win32(ret, "Failed to enumerate adapters".into()));
        }

        let mut current = buffer.as_ptr() as *const IP_ADAPTER_ADDRESSES_LH;
        while !current.is_null() {
            // SAFETY: Dereferencing verified linked list entry for network adapter descriptors.
            let adapter = unsafe { &*current };
            let desc = unsafe { adapter.Description.to_string() }.unwrap_or_default().to_lowercase();
            let friendly = unsafe { adapter.FriendlyName.to_string() }.unwrap_or_default().to_lowercase();

            if desc.contains("wintun") || friendly.contains("wintun") || friendly.contains("joyful") || desc.contains("tap-windows") || friendly.contains("openvpn") {
                let if_index = unsafe { adapter.Anonymous1.Anonymous.IfIndex };
                let luid = adapter.Luid;

                let mut resolved_adapter_ip = local_ip.unwrap_or(Ipv4Addr::new(0, 0, 0, 0));
                let mut current_unicast = adapter.FirstUnicastAddress;
                while !current_unicast.is_null() {
                    let unicast = unsafe { &*current_unicast };
                    if !unicast.Address.lpSockaddr.is_null() {
                        let sa = unsafe { &*unicast.Address.lpSockaddr };
                        if sa.sa_family == AF_INET {
                            let sin = unsafe { &*(unicast.Address.lpSockaddr as *const windows::Win32::Networking::WinSock::SOCKADDR_IN) };
                            let raw_ip = unsafe { sin.sin_addr.S_un.S_addr };
                            resolved_adapter_ip = Ipv4Addr::from(u32::from_ne_bytes(raw_ip.to_ne_bytes()));
                            break;
                        }
                    }
                    current_unicast = unicast.Next;
                }

                let resolved_gw = gateway_ip.unwrap_or(Ipv4Addr::new(0, 0, 0, 0));

                return Ok(Self {
                    interface_luid: luid,
                    interface_index: if_index,
                    adapter_ip: resolved_adapter_ip,
                    gateway_ip: resolved_gw,
                });
            }

            current = adapter.Next;
        }

        Err(VpnError::Routing("No active Wintun or OpenVPN network interface detected".into()))
    }

    pub fn add_host_route(&self, target_ip: Ipv4Addr) -> Result<(), VpnError> {
        #[cfg(windows)]
        use std::os::windows::process::CommandExt;

        let gw_candidates = [
            self.gateway_ip,
            self.adapter_ip,
            Ipv4Addr::new(0, 0, 0, 0),
        ];

        let mut route_added = false;
        for gw in &gw_candidates {
            if gw.is_unspecified() && *gw != Ipv4Addr::new(0, 0, 0, 0) {
                continue;
            }
            let mut cmd = Command::new("route");
            cmd.args([
                "-4",
                "add",
                &target_ip.to_string(),
                "mask",
                "255.255.255.255",
                &gw.to_string(),
                "metric",
                "5",
                "if",
                &self.interface_index.to_string(),
            ]);
            #[cfg(windows)]
            cmd.creation_flags(0x08000000);
            if let Ok(out) = cmd.output() {
                if out.status.success() {
                    route_added = true;
                    break;
                }
            }
        }

        let mut row = MIB_IPFORWARD_ROW2::default();
        row.InterfaceLuid = self.interface_luid;
        row.InterfaceIndex = self.interface_index;

        row.DestinationPrefix.Prefix.si_family = AF_INET;
        let dest_octets = target_ip.octets();
        row.DestinationPrefix.Prefix.Ipv4.sin_addr.S_un.S_addr = u32::from_ne_bytes(dest_octets);
        row.DestinationPrefix.PrefixLength = 32;

        row.NextHop.si_family = AF_INET;
        let gw_octets = if !self.gateway_ip.is_unspecified() {
            self.gateway_ip.octets()
        } else if !self.adapter_ip.is_unspecified() {
            self.adapter_ip.octets()
        } else {
            [0, 0, 0, 0]
        };
        row.NextHop.Ipv4.sin_addr.S_un.S_addr = u32::from_ne_bytes(gw_octets);

        row.Metric = 5;
        // SAFETY: Writing Win32 routing protocol identifier (MIB_IPPROTO_NETMGMT = 3) to Protocol field.
        unsafe {
            std::ptr::write_unaligned(
                std::ptr::addr_of_mut!(row.Protocol) as *mut i32,
                3,
            );
            let _ = CreateIpForwardEntry2(&row);
        }

        let mut flush_cmd = Command::new("ipconfig");
        flush_cmd.arg("/flushdns");
        #[cfg(windows)]
        flush_cmd.creation_flags(0x08000000);
        let _ = flush_cmd.output();

        if !route_added {
            // If route was already existing or added via Win32 IP Helper
            Ok(())
        } else {
            Ok(())
        }
    }

    pub fn remove_host_route(&self, target_ip: Ipv4Addr) -> Result<(), VpnError> {
        #[cfg(windows)]
        use std::os::windows::process::CommandExt;

        let mut route_cmd = Command::new("route");
        route_cmd.args(["delete", &target_ip.to_string()]);
        #[cfg(windows)]
        route_cmd.creation_flags(0x08000000);
        let _ = route_cmd.output();

        let mut row = MIB_IPFORWARD_ROW2::default();
        row.InterfaceLuid = self.interface_luid;
        row.InterfaceIndex = self.interface_index;

        row.DestinationPrefix.Prefix.si_family = AF_INET;
        let dest_octets = target_ip.octets();
        row.DestinationPrefix.Prefix.Ipv4.sin_addr.S_un.S_addr = u32::from_ne_bytes(dest_octets);
        row.DestinationPrefix.PrefixLength = 32;

        row.NextHop.si_family = AF_INET;
        let gw_octets = self.gateway_ip.octets();
        row.NextHop.Ipv4.sin_addr.S_un.S_addr = u32::from_ne_bytes(gw_octets);

        // SAFETY: Deleting host route entry using explicit prefix and next-hop descriptor.
        let _ = unsafe { DeleteIpForwardEntry2(&row) };

        let mut netsh_cmd = Command::new("netsh");
        netsh_cmd.args(["interface", "ip", "delete", "destinationcache"]);
        #[cfg(windows)]
        netsh_cmd.creation_flags(0x08000000);
        let _ = netsh_cmd.output();

        let mut ipconfig_cmd = Command::new("ipconfig");
        ipconfig_cmd.args(["/flushdns"]);
        #[cfg(windows)]
        ipconfig_cmd.creation_flags(0x08000000);
        let _ = ipconfig_cmd.output();

        Ok(())
    }
}
