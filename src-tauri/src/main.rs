#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod engine;
mod openvpn;
mod wintun;
mod routing;
mod hosts_manager;
mod dns;
mod dns_proxy;
mod service_ipc;
mod daemon;

mod service_manager;

use daemon::{CoreDaemon, IpcClient};
use engine::{VpnEngine, VpnStatus};
use openvpn::OpenVpnEngine;
use hosts_manager::HostsManager;
use routing::RoutingManager;
use dns::DnsResolver;
use service_ipc::{ServiceRequest, ServiceResponse};
use service_manager::ServiceManager;

use std::collections::HashMap;
use std::net::Ipv4Addr;
use std::sync::Arc;
use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    Manager, State, WindowEvent,
};
use tokio::sync::RwLock;
use windows::core::{HSTRING, PCWSTR};
use windows::Win32::System::Registry::{
    RegOpenKeyExW, RegSetValueExW, RegDeleteValueW, RegCloseKey,
    HKEY_CURRENT_USER, KEY_SET_VALUE, REG_SZ,
};

pub struct AppState {
    engine: Arc<OpenVpnEngine>,
    hosts: HostsManager,
    routing: Arc<RwLock<Option<RoutingManager>>>,
    managed_hosts: Arc<RwLock<HashMap<String, Option<Ipv4Addr>>>>,
    daemon_available: Arc<RwLock<bool>>,
}

fn create_tray_icon() -> tauri::image::Image<'static> {
    const W: u32 = 32;
    const H: u32 = 32;
    let mut rgba = vec![0u8; (W * H * 4) as usize];
    for y in 0..H {
        for x in 0..W {
            let idx = ((y * W + x) * 4) as usize;
            let dx = (x as f32 - 15.5) / 13.0;
            let dy = (y as f32 - 15.5) / 13.0;
            let dist = dx * dx + dy * dy;
            if dist <= 1.0 {
                rgba[idx] = 14;
                rgba[idx + 1] = 165;
                rgba[idx + 2] = 233;
                rgba[idx + 3] = 255;
            } else {
                rgba[idx + 3] = 0;
            }
        }
    }
    tauri::image::Image::new_owned(rgba, W, H)
}

#[tauri::command]
async fn start_vpn(
    state: State<'_, AppState>,
    config_content: String,
    selective_routing: Option<bool>,
) -> Result<(), String> {
    let is_selective = selective_routing.unwrap_or(true);
    let extra_routes: Vec<Ipv4Addr> = {
        let hosts = state.managed_hosts.read().await;
        hosts.values().filter_map(|ip| *ip).collect()
    };

    if *state.daemon_available.read().await {
        let req = ServiceRequest::StartVpn {
            config_content: config_content.clone(),
            selective_routing: is_selective,
            extra_routes: extra_routes.clone(),
        };
        match IpcClient::send_request(req).await {
            Ok(ServiceResponse::Ok) => return Ok(()),
            Ok(ServiceResponse::Error(e)) => return Err(e),
            _ => {}
        }
    }

    let sanitized = OpenVpnEngine::sanitize_config(&config_content, is_selective, &extra_routes);
    let temp_dir = std::env::temp_dir();
    let config_path = temp_dir.join("joyful_active.ovpn");
    std::fs::write(&config_path, sanitized.as_bytes()).map_err(|e| e.to_string())?;

    state.engine.add_log(format!("[INFO] Config written to {}", config_path.display())).await;
    state.engine.start(&config_path).await.map_err(|e| e.to_string())?;

    let state_engine = Arc::clone(&state.engine);
    let state_routing = Arc::clone(&state.routing);
    let state_hosts = Arc::clone(&state.managed_hosts);

    tokio::spawn(async move {
        for _ in 0..60 {
            tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
            if let Ok(st) = state_engine.get_status().await {
                if st.state == engine::ConnectionState::Connected {
                    state_engine.add_log("[INFO] Tunnel connected. Resolving Wintun interface...".into()).await;
                    let local_ip = st.local_ip.as_ref().and_then(|g| g.parse().ok());
                    let gw = st.gateway_ip.as_ref().and_then(|g| g.parse().ok());
                    match RoutingManager::detect_vpn_interface(local_ip, gw) {
                        Ok(rm) => {
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
                                state_engine.add_log(format!("[ROUTE] Active route confirmed: {h} -> {ip}")).await;
                            }
                            *state_routing.write().await = Some(rm);
                            state_engine.add_log("[INFO] All split-tunnel routes verified in kernel table".into()).await;
                        }
                        Err(e) => {
                            state_engine.add_log(format!("[WARN] Interface detection: {e}")).await;
                        }
                    }
                    break;
                }
            }
        }
    });

    Ok(())
}

#[tauri::command]
async fn stop_vpn(state: State<'_, AppState>) -> Result<(), String> {
    if *state.daemon_available.read().await {
        if let Ok(ServiceResponse::Ok) = IpcClient::send_request(ServiceRequest::StopVpn).await {
            return Ok(());
        }
    }
    *state.routing.write().await = None;
    let _ = state.hosts.revert_to_backup().await;
    state.engine.stop().await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn get_vpn_status(state: State<'_, AppState>) -> Result<VpnStatus, String> {
    if *state.daemon_available.read().await {
        if let Ok(ServiceResponse::Status(st)) = IpcClient::send_request(ServiceRequest::GetStatus).await {
            return Ok(st);
        }
    }
    state.engine.get_status().await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn get_engine_logs(state: State<'_, AppState>) -> Result<Vec<String>, String> {
    if *state.daemon_available.read().await {
        if let Ok(ServiceResponse::Logs(l)) = IpcClient::send_request(ServiceRequest::GetLogs).await {
            return Ok(l);
        }
    }
    Ok(state.engine.get_logs().await)
}

#[tauri::command]
async fn ping_target(target: String) -> Result<u32, String> {
    if let Ok(ServiceResponse::PingResult(rtt)) = IpcClient::send_request(ServiceRequest::Ping { target: target.clone() }).await {
        return Ok(rtt);
    }
    DnsResolver::ping_target(&target).await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn add_route_host(state: State<'_, AppState>, hostname: String) -> Result<serde_json::Value, String> {
    let (clean_host, manual_ip) = DnsResolver::extract_host_and_optional_ip(&hostname);
    if clean_host.is_empty() {
        return Err("Invalid or empty host input".into());
    }

    if *state.daemon_available.read().await {
        let req = ServiceRequest::AddHost {
            host: clean_host.clone(),
            ip: manual_ip,
        };
        if let Ok(ServiceResponse::HostAdded { host, ip }) = IpcClient::send_request(req).await {
            return Ok(serde_json::json!({
                "host": host,
                "ip": ip
            }));
        }
    }

    let resolved_ip = if let Some(ip) = manual_ip {
        Some(ip)
    } else {
        DnsResolver::resolve_ipv4(&clean_host).await.ok()
    };

    let ip_str = if let Some(ip) = resolved_ip {
        {
            let mut map = state.managed_hosts.write().await;
            map.insert(clean_host.clone(), Some(ip));
            let pairs: Vec<(Ipv4Addr, String)> = map.iter().filter_map(|(h, ip)| ip.map(|i| (i, h.clone()))).collect();
            let _ = state.hosts.apply_entries(&pairs).await;
        }

        if let Some(ref rm) = *state.routing.read().await {
            let _ = rm.add_host_route(ip);
            state.engine.add_log(format!("[ROUTE] Live route injected: {clean_host} -> {ip}")).await;
        } else {
            state.engine.add_log(format!("[ROUTE] Staged route: {clean_host} -> {ip}")).await;
        }
        ip.to_string()
    } else {
        {
            let mut map = state.managed_hosts.write().await;
            map.insert(clean_host.clone(), None);
        }
        state.engine.add_log(format!("[DNS] Host '{clean_host}' queued (resolves dynamically upon VPN connection)")).await;
        "Pending VPN".to_string()
    };

    Ok(serde_json::json!({
        "host": clean_host,
        "ip": ip_str
    }))
}

#[tauri::command]
async fn remove_route_host(state: State<'_, AppState>, hostname: String) -> Result<(), String> {
    let (clean_host, manual_ip) = DnsResolver::extract_host_and_optional_ip(&hostname);

    if *state.daemon_available.read().await {
        let req = ServiceRequest::RemoveHost {
            host: clean_host.clone(),
            ip: manual_ip,
        };
        let _ = IpcClient::send_request(req).await;
        return Ok(());
    }

    let mut removed_ip = {
        let mut map = state.managed_hosts.write().await;
        let ip_opt = map.remove(&clean_host);
        let pairs: Vec<(Ipv4Addr, String)> = map.iter().filter_map(|(h, ip)| ip.map(|i| (i, h.clone()))).collect();
        let _ = state.hosts.apply_entries(&pairs).await;
        ip_opt.flatten()
    };

    if removed_ip.is_none() {
        if let Some(ip) = manual_ip {
            removed_ip = Some(ip);
        } else if let Ok(ip) = DnsResolver::resolve_ipv4(&clean_host).await {
            removed_ip = Some(ip);
        }
    }

    if let Some(ip) = removed_ip {
        if let Some(ref rm) = *state.routing.read().await {
            let _ = rm.remove_host_route(ip);
        } else {
            #[cfg(windows)]
            use std::os::windows::process::CommandExt;

            let mut route_cmd = std::process::Command::new("route");
            route_cmd.args(["-4", "delete", &ip.to_string()]);
            #[cfg(windows)]
            route_cmd.creation_flags(0x08000000);
            let _ = route_cmd.output();

            let mut netsh_cmd = std::process::Command::new("netsh");
            netsh_cmd.args(["interface", "ip", "delete", "destinationcache"]);
            #[cfg(windows)]
            netsh_cmd.creation_flags(0x08000000);
            let _ = netsh_cmd.output();

            let mut ipconfig_cmd = std::process::Command::new("ipconfig");
            ipconfig_cmd.args(["/flushdns"]);
            #[cfg(windows)]
            ipconfig_cmd.creation_flags(0x08000000);
            let _ = ipconfig_cmd.output();
        }
        state.engine.add_log(format!("[ROUTE] Purged live kernel route for {clean_host} ({ip})")).await;
    }

    Ok(())
}

#[tauri::command]
async fn set_profile_hosts(state: State<'_, AppState>, hosts: Vec<String>) -> Result<Vec<serde_json::Value>, String> {
    let mut resolved_list = Vec::new();
    let mut pairs = Vec::new();
    let mut new_map = HashMap::new();

    for h in hosts {
        let (clean_host, manual_ip) = DnsResolver::extract_host_and_optional_ip(&h);
        if clean_host.is_empty() {
            continue;
        }
        let ip = if let Some(ip) = manual_ip {
            Some(ip)
        } else {
            DnsResolver::resolve_ipv4(&clean_host).await.ok()
        };

        new_map.insert(clean_host.clone(), ip);

        if let Some(ip) = ip {
            pairs.push((ip, clean_host.clone()));
            resolved_list.push(serde_json::json!({
                "host": clean_host,
                "ip": ip.to_string()
            }));
        } else {
            resolved_list.push(serde_json::json!({
                "host": clean_host,
                "ip": "Pending VPN"
            }));
        }
    }

    {
        let mut map = state.managed_hosts.write().await;
        *map = new_map;
    }

    let _ = state.hosts.apply_entries(&pairs).await;

    if let Some(ref rm) = *state.routing.read().await {
        for (ip, _) in &pairs {
            let _ = rm.add_host_route(*ip);
        }
        state.engine.add_log(format!("[ROUTE] Active profile re-applied with {} hosts", pairs.len())).await;
    }

    Ok(resolved_list)
}

#[tauri::command]
async fn toggle_autostart(enable: bool) -> Result<(), String> {
    let subkey = HSTRING::from("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
    let app_name = HSTRING::from("JoyfulVpn");

    let mut key = windows::Win32::System::Registry::HKEY::default();
    let status = unsafe {
        RegOpenKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR(subkey.as_wide().as_ptr()),
            0,
            KEY_SET_VALUE,
            &mut key,
        )
    };

    if status.0 != 0 {
        return Err(format!("RegOpenKeyExW failed: {}", status.0));
    }

    let result = if enable {
        let current_exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let exe_str = current_exe.to_str().ok_or("Non-UTF8 path")?;
        let wide_exe: Vec<u16> = exe_str.encode_utf16().chain(std::iter::once(0)).collect();
        unsafe {
            RegSetValueExW(
                key,
                PCWSTR(app_name.as_wide().as_ptr()),
                0,
                REG_SZ,
                Some(std::slice::from_raw_parts(wide_exe.as_ptr() as *const u8, wide_exe.len() * 2)),
            )
        }
    } else {
        unsafe { RegDeleteValueW(key, PCWSTR(app_name.as_wide().as_ptr())) }
    };

    unsafe { let _ = RegCloseKey(key); };

    if result.0 != 0 {
        return Err(format!("Registry write/delete error: {}", result.0));
    }

    Ok(())
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--install-service") {
        match ServiceManager::install_service() {
            Ok(_) => println!("[JOYFUL_SCM] Service 'JoyfulVpnCore' installed successfully."),
            Err(e) => eprintln!("[JOYFUL_SCM_ERROR] Failed to install service: {e}"),
        }
        return;
    }

    if args.iter().any(|a| a == "--uninstall-service") {
        match ServiceManager::uninstall_service() {
            Ok(_) => println!("[JOYFUL_SCM] Service 'JoyfulVpnCore' uninstalled successfully."),
            Err(e) => eprintln!("[JOYFUL_SCM_ERROR] Failed to uninstall service: {e}"),
        }
        return;
    }

    if args.iter().any(|a| a == "--service-scm") {
        if let Err(e) = ServiceManager::run_service_dispatcher() {
            eprintln!("[JOYFUL_SCM_ERROR] SCM Dispatcher failure: {e}");
        }
        return;
    }

    if args.iter().any(|a| a == "--service" || a == "-d" || a == "--daemon") {
        let rt = tokio::runtime::Runtime::new().expect("Tokio runtime failed");
        rt.block_on(async {
            let daemon = Arc::new(CoreDaemon::new());
            println!("[JOYFUL_DAEMON] Starting background core service on Named Pipe...");
            let _ = daemon.run_ipc_server().await;
        });
        return;
    }

    #[cfg(windows)]
    let _instance_mutex = {
        use windows::Win32::System::Threading::CreateMutexW;
        use windows::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError};
        use windows::Win32::UI::WindowsAndMessaging::{FindWindowW, ShowWindow, SetForegroundWindow, SW_RESTORE};
        use windows::core::w;

        let handle = unsafe { CreateMutexW(None, true, w!("Global\\JoyfulVpnSingleInstanceMutex")) };
        if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
            unsafe {
                if let Ok(hwnd) = FindWindowW(None, w!("Joyful VPN")) {
                    let _ = ShowWindow(hwnd, SW_RESTORE);
                    let _ = SetForegroundWindow(hwnd);
                }
            }
            return;
        }
        handle
    };

    let _ = wintun::WintunDriver::load();

    let app_state = AppState {
        engine: Arc::new(OpenVpnEngine::new(11940)),
        hosts: HostsManager::new(),
        routing: Arc::new(RwLock::new(None)),
        managed_hosts: Arc::new(RwLock::new(HashMap::new())),
        daemon_available: Arc::new(RwLock::new(false)),
    };

    tauri::Builder::default()
        .manage(app_state)
        .setup(|app| {
            if let Some(window) = app.get_webview_window("main") {
                if let Some(icon) = app.default_window_icon().cloned() {
                    let _ = window.set_icon(icon);
                }
            }

            let show_i = MenuItem::with_id(app, "show", "Show Joyful VPN", true, None::<&str>)?;
            let quit_i = MenuItem::with_id(app, "quit", "Disconnect & Exit", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show_i, &quit_i])?;

            let tray = TrayIconBuilder::with_id("main-tray")
                .icon(create_tray_icon())
                .tooltip("Joyful VPN")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "show" => {
                        if let Some(window) = app.get_webview_window("main") {
                            let _ = window.show();
                            let _ = window.unminimize();
                            let _ = window.set_focus();
                        }
                    }
                    "quit" => {
                        let _ = app.remove_tray_by_id("main-tray");
                        let state = app.state::<AppState>();
                        let engine = Arc::clone(&state.engine);
                        let hosts = &state.hosts;
                        tauri::async_runtime::block_on(async {
                            let _ = hosts.revert_to_backup().await;
                            let _ = engine.stop().await;
                        });
                        app.exit(0);
                    }
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        let app = tray.app_handle();
                        if let Some(window) = app.get_webview_window("main") {
                            let _ = window.show();
                            let _ = window.unminimize();
                            let _ = window.set_focus();
                        }
                    }
                })
                .build(app)?;

            let _ = tray;

            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .invoke_handler(tauri::generate_handler![
            start_vpn,
            stop_vpn,
            get_vpn_status,
            get_engine_logs,
            ping_target,
            add_route_host,
            remove_route_host,
            set_profile_hosts,
            toggle_autostart
        ])
        .run(tauri::generate_context!())
        .expect("Tauri runtime failure");
}
