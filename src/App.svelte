<script lang="ts">
  import { onMount, onDestroy } from 'svelte';
  import { 
    Shield, 
    Power, 
    Activity, 
    Network, 
    FileCode, 
    Plus, 
    Trash2, 
    CheckCircle2, 
    AlertCircle,
    Upload,
    Download,
    Save,
    Zap,
    FolderKanban,
    Edit2,
    Check
  } from 'lucide-svelte';
  import type { VpnStatus, VpnProfile, SplitRouteHost } from './lib/types';

  let status = $state<VpnStatus>({
    state: 'Disconnected',
    bytes_in: 0,
    bytes_out: 0,
    connected_duration_sec: 0,
    local_ip: null,
    gateway_ip: null
  });

  let profiles = $state<VpnProfile[]>([
    {
      id: 'default',
      name: 'Default Profile',
      configPath: 'active_config.ovpn',
      configContent: 'client\ndev tun\nproto udp\nremote vpn.example.com 1194\nresolv-retry infinite\nnobind\npersist-key\npersist-tun\n',
      hosts: []
    }
  ]);

  let activeProfileId = $state('default');
  let editingProfileId = $state<string | null>(null);
  let editingProfileName = $state('');

  let newHost = $state('');
  let autostart = $state(false);
  let isActionPending = $state(false);
  let globalPing = $state<number | null>(null);
  let isPingingGlobal = $state(false);
  let saveFeedback = $state(false);
  let intervalId: any = null;
  let logs = $state<string[]>([
    '[INFO] Joyful VPN interface initialized.',
  ]);

  const activeProfile = $derived(
    profiles.find(p => p.id === activeProfileId) || profiles[0] || {
      id: 'default',
      name: 'Default Profile',
      configPath: 'active_config.ovpn',
      configContent: '',
      hosts: []
    }
  );

  const isTauri = typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window;
  const isConnected = $derived(status.state === 'Connected');
  const isConnecting = $derived(status.state === 'Connecting' || status.state === 'Reconnecting');
  const canDisconnect = $derived(isConnected || isConnecting || isActionPending);

  const formattedDuration = $derived.by(() => {
    const s = status.connected_duration_sec;
    const hrs = Math.floor(s / 3600);
    const mins = Math.floor((s % 3600) / 60);
    const secs = s % 60;
    return `${hrs.toString().padStart(2, '0')}:${mins.toString().padStart(2, '0')}:${secs.toString().padStart(2, '0')}`;
  });

  function addLog(msg: string) {
    logs = [...logs, `[${new Date().toLocaleTimeString()}] ${msg}`];
  }

  async function callTauri<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
    if (!isTauri) {
      throw new Error('Running in external web browser. Tauri desktop runtime is not connected.');
    }
    const { invoke } = await import('@tauri-apps/api/core');
    return invoke<T>(cmd, args);
  }

  function persistProfiles() {
    try {
      localStorage.setItem('joyful_vpn_profiles', JSON.stringify(profiles));
      localStorage.setItem('joyful_vpn_active_profile_id', activeProfileId);
    } catch {}
  }

  function handleSaveClick() {
    persistProfiles();
    saveFeedback = true;
    setTimeout(() => { saveFeedback = false; }, 2000);
    addLog(`Profile "${activeProfile.name}" saved locally.`);
  }

  function handleExportConfig() {
    if (!activeProfile.configContent.trim()) {
      alert('Config editor is empty.');
      return;
    }
    const blob = new Blob([activeProfile.configContent], { type: 'text/plain;charset=utf-8' });
    const url = URL.createObjectURL(blob);
    const a = document.createElement('a');
    a.href = url;
    const filename = activeProfile.configPath.endsWith('.ovpn') 
      ? activeProfile.configPath 
      : `${activeProfile.name.toLowerCase().replace(/[^a-z0-9_-]/g, '_')}.ovpn`;
    a.download = filename;
    a.click();
    URL.revokeObjectURL(url);
    addLog(`Exported configuration to ${a.download}`);
  }

  async function pollStatus() {
    if (!isTauri) return;
    try {
      status = await callTauri<VpnStatus>('get_vpn_status');
      const backendLogs = await callTauri<string[]>('get_engine_logs');
      if (backendLogs && backendLogs.length > 0) {
        let changed = false;
        for (const l of backendLogs) {
          if (!logs.includes(l)) {
            logs.push(l);
            changed = true;
          }
        }
        if (changed) {
          logs = [...logs];
        }
      }
    } catch {}
  }

  async function handleToggleConnection() {
    if (!activeProfile.configContent.trim()) {
      alert('Please import, paste, or load a valid .ovpn configuration first.');
      return;
    }

    persistProfiles();

    try {
      if (canDisconnect) {
        addLog('Stopping VPN connection...');
        isActionPending = true;
        await callTauri('stop_vpn');
        status.state = 'Disconnected';
        isActionPending = false;
        addLog('VPN connection stopped.');
      } else {
        addLog(`Initiating connection (${activeProfile.name})...`);
        isActionPending = true;
        status.state = 'Connecting';
        const hostStrs = activeProfile.hosts.map(h => `${h.host} ${h.ip}`.trim());
        await callTauri('set_profile_hosts', { hosts: hostStrs });
        await callTauri('start_vpn', { configContent: activeProfile.configContent, selectiveRouting: true });
        isActionPending = false;
        addLog('OpenVPN process spawned.');
      }
      await pollStatus();
    } catch (err: any) {
      isActionPending = false;
      const msg = err?.message || String(err);
      addLog(`[ERROR] ${msg}`);
      alert(msg);
      await pollStatus();
    }
  }

  function handleFileUpload(event: Event) {
    const input = event.target as HTMLInputElement;
    if (input.files && input.files[0]) {
      const file = input.files[0];
      const reader = new FileReader();
      reader.onload = (e) => {
        const content = (e.target?.result as string) || '';
        profiles = profiles.map(p => 
          p.id === activeProfileId ? { ...p, configPath: file.name, configContent: content } : p
        );
        persistProfiles();
        addLog(`Imported configuration into "${activeProfile.name}": ${file.name}`);
      };
      reader.readAsText(file);
    }
  }

  async function handleAddProfile() {
    const newId = 'profile_' + Date.now();
    const newName = `Profile ${profiles.length + 1}`;
    const newProf: VpnProfile = {
      id: newId,
      name: newName,
      configPath: 'client.ovpn',
      configContent: 'client\ndev tun\nproto udp\nremote vpn.example.com 1194\nresolv-retry infinite\nnobind\npersist-key\npersist-tun\n',
      hosts: []
    };
    profiles = [...profiles, newProf];
    activeProfileId = newId;
    persistProfiles();
    addLog(`Created new profile: ${newName}`);
  }

  async function handleSwitchProfile(profileId: string) {
    if (profileId === activeProfileId) return;
    if (canDisconnect) {
      const ok = confirm(`VPN is active on "${activeProfile.name}". Disconnect and switch to selected profile?`);
      if (!ok) return;
      await callTauri('stop_vpn');
      status.state = 'Disconnected';
    }
    activeProfileId = profileId;
    persistProfiles();
    addLog(`Switched active profile: ${activeProfile.name}`);
    if (isTauri) {
      try {
        const hostStrs = activeProfile.hosts.map(h => `${h.host} ${h.ip}`.trim());
        await callTauri('set_profile_hosts', { hosts: hostStrs });
      } catch {}
    }
  }

  async function handleDeleteProfile(e: MouseEvent, profileId: string) {
    e.stopPropagation();
    if (profiles.length <= 1) {
      alert('Cannot delete the last remaining profile.');
      return;
    }
    const prof = profiles.find(p => p.id === profileId);
    if (!prof) return;
    if (!confirm(`Delete profile "${prof.name}"?`)) return;

    if (activeProfileId === profileId && canDisconnect) {
      await callTauri('stop_vpn');
      status.state = 'Disconnected';
    }

    profiles = profiles.filter(p => p.id !== profileId);
    if (activeProfileId === profileId) {
      activeProfileId = profiles[0].id;
    }
    persistProfiles();
    addLog(`Deleted profile: ${prof.name}`);
  }

  function startRenameProfile(e: MouseEvent, profile: VpnProfile) {
    e.stopPropagation();
    editingProfileId = profile.id;
    editingProfileName = profile.name;
  }

  function saveRenameProfile() {
    if (!editingProfileId) return;
    const trimmed = editingProfileName.trim();
    if (trimmed) {
      profiles = profiles.map(p => p.id === editingProfileId ? { ...p, name: trimmed } : p);
      persistProfiles();
    }
    editingProfileId = null;
  }

  async function handleAddHost() {
    const raw = newHost.trim();
    if (!raw) return;

    try {
      const result = await callTauri<{ host: string; ip: string }>('add_route_host', { hostname: raw });
      const updatedHosts = [
        ...activeProfile.hosts.filter(h => h.host !== result.host),
        { host: result.host, ip: result.ip, ping: null }
      ];
      profiles = profiles.map(p => p.id === activeProfileId ? { ...p, hosts: updatedHosts } : p);
      persistProfiles();
      addLog(`Added host: ${result.host} (${result.ip})`);
      newHost = '';
    } catch (e: any) {
      const msg = e?.message || String(e);
      let fallbackHost = raw;
      if (fallbackHost.includes('://')) fallbackHost = fallbackHost.split('://')[1];
      if (fallbackHost.includes('@')) fallbackHost = fallbackHost.split('@')[1];
      fallbackHost = fallbackHost.split('/')[0].split('?')[0].split(':')[0].trim().toLowerCase();

      if (fallbackHost) {
        const updatedHosts = [
          ...activeProfile.hosts.filter(h => h.host !== fallbackHost),
          { host: fallbackHost, ip: 'Pending VPN', ping: null }
        ];
        profiles = profiles.map(p => p.id === activeProfileId ? { ...p, hosts: updatedHosts } : p);
        persistProfiles();
        addLog(`Added pending route: ${fallbackHost} (DNS resolves on connection)`);
        newHost = '';
      } else {
        addLog(`[ERROR] Invalid host: ${msg}`);
      }
    }
  }

  async function handleRemoveHost(hostname: string) {
    try {
      await callTauri('remove_route_host', { hostname });
      const updatedHosts = activeProfile.hosts.filter(h => h.host !== hostname);
      profiles = profiles.map(p => p.id === activeProfileId ? { ...p, hosts: updatedHosts } : p);
      persistProfiles();
      addLog(`Removed host route: ${hostname}`);
    } catch (e: any) {
      const msg = e?.message || String(e);
      addLog(`[ERROR] Host removal failed: ${msg}`);
      alert(`Removal error: ${msg}`);
    }
  }

  async function handlePingHost(item: SplitRouteHost) {
    try {
      addLog(`Pinging ${item.host} (${item.ip})...`);
      const latency = await callTauri<number>('ping_target', { target: item.ip });
      const updatedHosts = activeProfile.hosts.map(h => h.host === item.host ? { ...h, ping: latency } : h);
      profiles = profiles.map(p => p.id === activeProfileId ? { ...p, hosts: updatedHosts } : p);
      addLog(`Ping response from ${item.host}: ${latency} ms`);
    } catch (e: any) {
      const updatedHosts = activeProfile.hosts.map(h => h.host === item.host ? { ...h, ping: -1 } : h);
      profiles = profiles.map(p => p.id === activeProfileId ? { ...p, hosts: updatedHosts } : p);
      addLog(`Ping failed for ${item.host}: ${e?.message || e}`);
    }
  }

  async function handlePingGlobal() {
    isPingingGlobal = true;
    try {
      const target = status.gateway_ip || '1.1.1.1';
      const latency = await callTauri<number>('ping_target', { target });
      globalPing = latency;
      addLog(`Gateway latency (${target}): ${latency} ms`);
    } catch (e: any) {
      globalPing = -1;
      addLog(`Gateway probe failed: ${e?.message || e}`);
    } finally {
      isPingingGlobal = false;
    }
  }

  async function handleAutostartToggle() {
    try {
      autostart = !autostart;
      await callTauri('toggle_autostart', { enable: autostart });
      addLog(`Autostart toggled: ${autostart ? 'ENABLED' : 'DISABLED'}`);
    } catch (e: any) {
      autostart = !autostart;
      const msg = e?.message || String(e);
      addLog(`[ERROR] Autostart error: ${msg}`);
    }
  }

  onMount(async () => {
    try {
      const savedProfilesJson = localStorage.getItem('joyful_vpn_profiles');
      const savedActiveId = localStorage.getItem('joyful_vpn_active_profile_id');
      if (savedProfilesJson) {
        profiles = JSON.parse(savedProfilesJson);
        if (savedActiveId && profiles.some(p => p.id === savedActiveId)) {
          activeProfileId = savedActiveId;
        } else if (profiles.length > 0) {
          activeProfileId = profiles[0].id;
        }
      } else {
        const legacyConfig = localStorage.getItem('joyful_vpn_config');
        const legacyName = localStorage.getItem('joyful_vpn_config_name');
        const legacyHosts = localStorage.getItem('joyful_vpn_hosts');
        profiles = [{
          id: 'default',
          name: 'Default Profile',
          configPath: legacyName || 'client.ovpn',
          configContent: legacyConfig || 'client\ndev tun\nproto udp\nremote vpn.example.com 1194\nresolv-retry infinite\nnobind\npersist-key\npersist-tun\n',
          hosts: legacyHosts ? JSON.parse(legacyHosts) : []
        }];
        activeProfileId = 'default';
        persistProfiles();
      }
    } catch {}

    if (isTauri && activeProfile) {
      try {
        const hostStrs = activeProfile.hosts.map(h => `${h.host} ${h.ip}`.trim());
        await callTauri('set_profile_hosts', { hosts: hostStrs });
      } catch {}
    }

    intervalId = setInterval(pollStatus, 1000);
  });

  onDestroy(() => {
    if (intervalId) clearInterval(intervalId);
  });
</script>

<main class="h-screen w-screen bg-slate-950 text-slate-100 flex flex-col select-none font-sans overflow-hidden">
  {#if !isTauri}
    <div class="bg-amber-500/10 border-b border-amber-500/30 px-6 py-1.5 flex items-center justify-between text-xs text-amber-400">
      <div class="flex items-center gap-2">
        <AlertCircle class="w-4 h-4 shrink-0" />
        <span>Вы открыли веб-версию в браузере. Вызовы Win32 FFI и управление OpenVPN выполняются из нативного десктопного окна Tauri.</span>
      </div>
      <span class="font-mono text-[11px] opacity-75">Browser Mode</span>
    </div>
  {/if}

  <header class="h-14 border-b border-slate-800 flex items-center justify-between px-6 bg-slate-900/50 backdrop-blur">
    <div class="flex items-center gap-3">
      <Shield class="w-6 h-6 text-sky-400" />
      <span class="font-bold tracking-tight text-lg">Joyful VPN</span>
      <span class="text-xs px-2 py-0.5 rounded border border-sky-500/30 bg-sky-500/10 text-sky-400">WSL2 Mirrored</span>
      <span class="text-xs px-2 py-0.5 rounded border border-emerald-500/30 bg-emerald-500/10 text-emerald-400 font-medium">
        {activeProfile.name}
      </span>
    </div>

    <div class="flex items-center gap-4">
      <button 
        onclick={handleAutostartToggle}
        class="text-xs px-3 py-1.5 rounded-md border border-slate-700 bg-slate-800 hover:bg-slate-700 transition"
      >
        Autostart: {autostart ? 'ON' : 'OFF'}
      </button>

      <button
        onclick={handleToggleConnection}
        class="flex items-center gap-2 px-5 py-2 rounded-lg font-medium text-sm transition shadow-lg {canDisconnect ? 'bg-rose-600 hover:bg-rose-500 text-white shadow-rose-950/50' : 'bg-emerald-600 hover:bg-emerald-500 text-white shadow-emerald-950/50'}"
      >
        <Power class="w-4 h-4" />
        {canDisconnect ? 'Disconnect' : 'Connect'}
      </button>
    </div>
  </header>

  <section class="flex-1 p-5 grid grid-cols-12 gap-5 overflow-hidden min-h-0">
    <div class="col-span-4 flex flex-col gap-4 min-h-0">
      <div class="p-4 rounded-xl bg-slate-900 border border-slate-800 shadow-sm shrink-0">
        <div class="flex items-center justify-between mb-3">
          <h2 class="text-[11px] font-semibold text-slate-400 uppercase tracking-wider">Connection State</h2>
          <button
            onclick={handlePingGlobal}
            disabled={isPingingGlobal}
            class="text-[10px] px-2 py-0.5 rounded border border-slate-700 bg-slate-800 hover:bg-slate-700 text-slate-300 flex items-center gap-1 transition"
          >
            <Zap class="w-3 h-3 text-amber-400" />
            {#if isPingingGlobal}
              <span>Testing...</span>
            {:else if globalPing !== null}
              <span class={globalPing < 0 ? 'text-rose-400' : 'text-emerald-400'}>{globalPing < 0 ? 'Timeout' : `${globalPing} ms`}</span>
            {:else}
              <span>Ping RTT</span>
            {/if}
          </button>
        </div>

        <div class="flex items-center justify-between mb-3">
          <div class="flex items-center gap-2">
            {#if isConnected}
              <CheckCircle2 class="w-4 h-4 text-emerald-400" />
            {:else if isConnecting}
              <div class="w-3.5 h-3.5 border-2 border-sky-400 border-t-transparent rounded-full animate-spin"></div>
            {:else}
              <AlertCircle class="w-4 h-4 text-slate-500" />
            {/if}
            <span class="font-medium text-sm">{status.state}</span>
          </div>
          <span class="font-mono text-xs text-slate-300">{formattedDuration}</span>
        </div>

        <div class="grid grid-cols-2 gap-2 pt-2.5 border-t border-slate-800/80 text-[11px]">
          <div>
            <span class="text-slate-500 block">Assigned IP</span>
            <span class="font-mono text-slate-300">{status.local_ip ?? '—'}</span>
          </div>
          <div>
            <span class="text-slate-500 block">Gateway</span>
            <span class="font-mono text-slate-300">{status.gateway_ip ?? '—'}</span>
          </div>
          <div>
            <span class="text-slate-500 block">Traffic In</span>
            <span class="font-mono text-slate-300">{(status.bytes_in / (1024 * 1024)).toFixed(2)} MB</span>
          </div>
          <div>
            <span class="text-slate-500 block">Traffic Out</span>
            <span class="font-mono text-slate-300">{(status.bytes_out / (1024 * 1024)).toFixed(2)} MB</span>
          </div>
        </div>
      </div>

      <div class="p-4 rounded-xl bg-slate-900 border border-slate-800 flex flex-col shrink-0">
        <div class="flex items-center justify-between mb-2.5">
          <div class="flex items-center gap-1.5">
            <FolderKanban class="w-3.5 h-3.5 text-sky-400" />
            <h2 class="text-[11px] font-semibold text-slate-400 uppercase tracking-wider">Profiles List</h2>
          </div>
          <button
            onclick={handleAddProfile}
            class="text-[10px] px-2 py-0.5 rounded bg-sky-600 hover:bg-sky-500 text-white flex items-center gap-1 transition"
          >
            <Plus class="w-3 h-3" />
            <span>New Profile</span>
          </button>
        </div>

        <div class="space-y-1.5 max-h-36 overflow-y-auto pr-1">
          {#each profiles as profile (profile.id)}
            <div
              tabindex="0"
              role="button"
              onclick={() => handleSwitchProfile(profile.id)}
              onkeydown={(e) => { if (e.key === 'Enter') handleSwitchProfile(profile.id); }}
              class="w-full text-left p-2 rounded-lg border transition flex items-center justify-between text-xs cursor-pointer {profile.id === activeProfileId ? 'bg-sky-950/40 border-sky-500/60 text-sky-200' : 'bg-slate-950/40 border-slate-800/80 hover:bg-slate-800/40 text-slate-300'}"
            >
              <div class="flex items-center gap-2 truncate mr-2">
                <div class="w-2 h-2 rounded-full shrink-0 {profile.id === activeProfileId ? 'bg-emerald-400 shadow-sm shadow-emerald-400/50' : 'bg-slate-600'}"></div>
                {#if editingProfileId === profile.id}
                  <input
                    type="text"
                    bind:value={editingProfileName}
                    onkeydown={(e) => { if (e.key === 'Enter') saveRenameProfile(); }}
                    onclick={(e) => e.stopPropagation()}
                    class="bg-slate-950 border border-sky-400 rounded px-1.5 py-0.5 text-xs text-white outline-none w-32"
                  />
                  <button onclick={(e) => { e.stopPropagation(); saveRenameProfile(); }} class="p-0.5 hover:text-emerald-400">
                    <Check class="w-3 h-3" />
                  </button>
                {:else}
                  <span class="font-medium truncate">{profile.name}</span>
                  <span class="text-[10px] text-slate-500 font-mono">({profile.hosts.length} routes)</span>
                {/if}
              </div>

              <div class="flex items-center gap-1 shrink-0">
                {#if editingProfileId !== profile.id}
                  <button 
                    onclick={(e) => startRenameProfile(e, profile)}
                    class="p-1 text-slate-500 hover:text-sky-300 transition"
                    title="Rename profile"
                  >
                    <Edit2 class="w-3 h-3" />
                  </button>
                {/if}
                {#if profiles.length > 1}
                  <button
                    onclick={(e) => handleDeleteProfile(e, profile.id)}
                    class="p-1 text-slate-500 hover:text-rose-400 transition"
                    title="Delete profile"
                  >
                    <Trash2 class="w-3 h-3" />
                  </button>
                {/if}
              </div>
            </div>
          {/each}
        </div>
      </div>

      <div class="flex-1 p-4 rounded-xl bg-slate-900 border border-slate-800 flex flex-col min-h-0">
        <div class="flex items-center justify-between mb-2.5">
          <div class="flex items-center gap-1.5">
            <Network class="w-3.5 h-3.5 text-sky-400" />
            <h2 class="text-[11px] font-semibold text-slate-400 uppercase tracking-wider">Split-Tunnel Routes</h2>
          </div>
          <span class="text-[10px] text-slate-500">DB URI / Host / URL</span>
        </div>

        <div class="flex gap-2 mb-3">
          <input
            type="text"
            bind:value={newHost}
            onkeydown={(e) => { if (e.key === 'Enter') handleAddHost(); }}
            placeholder="mysql-rw-delivery-production.psvc.internal or URL"
            class="flex-1 bg-slate-950 border border-slate-800 rounded-lg px-2.5 py-1.5 text-xs text-slate-200 outline-none focus:border-sky-500 placeholder:text-slate-600"
          />
          <button
            onclick={handleAddHost}
            class="px-3 py-1.5 bg-sky-600 hover:bg-sky-500 text-white rounded-lg text-xs font-medium flex items-center gap-1 transition shrink-0"
          >
            <Plus class="w-3.5 h-3.5" />
            Add
          </button>
        </div>

        <div class="flex-1 overflow-y-auto space-y-1.5 pr-1 min-h-0">
          {#each activeProfile.hosts as item (item.host)}
            <div class="flex items-center justify-between p-2 rounded-lg bg-slate-950/60 border border-slate-800/60 text-xs">
              <div class="flex flex-col truncate mr-2">
                <span class="font-medium text-slate-200 truncate">{item.host}</span>
                <span class="font-mono text-[10px] text-slate-500">{item.ip}</span>
              </div>
              <div class="flex items-center gap-1.5 shrink-0">
                <button
                  onclick={() => handlePingHost(item)}
                  class="px-1.5 py-0.5 rounded bg-slate-800 hover:bg-slate-700 text-[10px] font-mono transition flex items-center gap-1"
                >
                  <Zap class="w-2.5 h-2.5 text-amber-400" />
                  {#if item.ping !== undefined && item.ping !== null}
                    <span class={item.ping < 0 ? 'text-rose-400' : 'text-emerald-400'}>{item.ping < 0 ? 'Timeout' : `${item.ping}ms`}</span>
                  {:else}
                    <span class="text-slate-400">Ping</span>
                  {/if}
                </button>
                <button
                  onclick={() => handleRemoveHost(item.host)}
                  class="p-1 text-slate-500 hover:text-rose-400 transition"
                  title="Remove from split-tunnel"
                >
                  <Trash2 class="w-3.5 h-3.5" />
                </button>
              </div>
            </div>
          {/each}
          {#if activeProfile.hosts.length === 0}
            <div class="h-24 flex items-center justify-center text-xs text-slate-600">
              No split-tunnel routes in {activeProfile.name}
            </div>
          {/if}
        </div>
      </div>
    </div>

    <div class="col-span-8 flex flex-col gap-4 min-h-0">
      <div class="flex-1 p-5 rounded-xl bg-slate-900 border border-slate-800 flex flex-col min-h-0">
        <div class="flex items-center justify-between mb-3">
          <div class="flex items-center gap-2">
            <FileCode class="w-4 h-4 text-emerald-400" />
            <h2 class="text-xs font-semibold text-slate-400 uppercase tracking-wider">
              {activeProfile.name} Configuration (.ovpn)
            </h2>
          </div>
          <div class="flex items-center gap-2">
            <button
              onclick={handleSaveClick}
              class="text-[11px] px-2.5 py-1 rounded bg-slate-800 hover:bg-slate-700 text-slate-300 flex items-center gap-1 transition"
            >
              <Save class="w-3 h-3 text-sky-400" />
              <span>{saveFeedback ? 'Saved!' : 'Save'}</span>
            </button>
            <button
              onclick={handleExportConfig}
              class="text-[11px] px-2.5 py-1 rounded bg-slate-800 hover:bg-slate-700 text-slate-300 flex items-center gap-1 transition"
            >
              <Download class="w-3 h-3 text-amber-400" />
              <span>Export</span>
            </button>
            <label class="cursor-pointer text-[11px] px-2.5 py-1 rounded bg-slate-800 hover:bg-slate-700 text-slate-300 flex items-center gap-1 transition">
              <Upload class="w-3 h-3 text-emerald-400" />
              <span>Import</span>
              <input type="file" accept=".ovpn,.conf,.txt" onchange={handleFileUpload} class="hidden" />
            </label>
            <span class="text-[11px] font-mono text-slate-500 max-w-[120px] truncate">{activeProfile.configPath}</span>
          </div>
        </div>

        <textarea
          bind:value={activeProfile.configContent}
          oninput={persistProfiles}
          spellcheck="false"
          placeholder="Paste or import your .ovpn configuration here..."
          class="flex-1 w-full bg-slate-950 font-mono text-xs text-slate-300 p-4 rounded-lg border border-slate-800 resize-none outline-none focus:border-emerald-500/50 leading-relaxed"
        ></textarea>
      </div>

      <div class="h-40 p-4 rounded-xl bg-slate-900 border border-slate-800 flex flex-col shrink-0">
        <div class="flex items-center justify-between mb-2">
          <div class="flex items-center gap-2">
            <Activity class="w-4 h-4 text-amber-400" />
            <h2 class="text-xs font-semibold text-slate-400 uppercase tracking-wider">Engine Log Stream</h2>
          </div>
          <span class="text-[10px] text-slate-500">Крестик [X] сворачивает приложение в трей Windows</span>
        </div>
        <div class="flex-1 overflow-y-auto font-mono text-[11px] text-slate-400 space-y-1">
          {#each logs as logLine}
            <div class={logLine.includes('[ERROR]') || logLine.includes('[VPN ERR]') ? 'text-rose-400' : logLine.includes('[WARN]') ? 'text-amber-400' : logLine.includes('[ROUTE]') ? 'text-sky-400' : 'text-slate-300'}>
              {logLine}
            </div>
          {/each}
        </div>
      </div>
    </div>
  </section>
</main>
