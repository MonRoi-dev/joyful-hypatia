# ARCHITECTURAL ALTERNATIVE IMPLEMENTATION PLAN

## 1. Local DNS Resolver Proxy (`src-tauri/src/dns_proxy.rs`)
- Lightweight UDP DNS Server binding on `127.0.0.1:53` (or ephemeral `127.0.0.1:5353` with upstream fallback).
- Inspects incoming DNS queries for Question QNAME.
- Maintains dynamic sync with split-tunnel routing rules.
- If QNAME matches a split-tunnel domain:
  - If a cached/static IP is assigned, crafts a DNS Answer (A record) pointing directly to that target IP with TTL = 10s.
  - If dynamic resolution needed, queries the upstream VPN DNS or system fallback, caches result, and injects route kernel-level.
- If QNAME does NOT match:
  - Proxies transparently to upstream system DNS (e.g. 1.1.1.1, 8.8.8.8, or primary adapter DNS) and forwards response back to client.
- Zero need to tamper with or corrupt `C:\Windows\System32\drivers\etc\hosts`.

## 2. Windows Service & IPC Decoupling Architecture (`src-tauri/src/service_ipc.rs` + Daemon Mode)
- **Unified Binary Dual Mode**:
  - `joyful-hypatia.exe --service`: Starts in background as Windows Service / Daemon mode, listening on Local Named Pipe `\\.\pipe\joyful-vpn-core`.
  - Normal execution: Starts GUI (Tauri). Connects to Named Pipe if daemon service is present; falls back seamlessly to embedded core if service is running locally or standalone.
- **Strict Protocol Contract (`IpcMessage`)**:
  - `StartVpn { config_content, extra_routes, is_selective }`
  - `StopVpn`
  - `GetStatus -> VpnStatus`
  - `AddHostRoute { host, ip } -> AddHostResult`
  - `RemoveHostRoute { host, ip }`
  - `GetLogs -> Vec<String>`
- Decouples UI thread, prevents UAC elevation bugs for GUI, eliminates race conditions with file handles.
