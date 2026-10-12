export type ConnectionState =
  | 'Disconnected'
  | 'Connecting'
  | 'Connected'
  | 'Reconnecting'
  | 'Disconnecting'
  | { Error: string };

export interface VpnStatus {
  state: ConnectionState;
  bytes_in: number;
  bytes_out: number;
  connected_duration_sec: number;
  local_ip: string | null;
  gateway_ip: string | null;
}

export interface SplitRouteHost {
  host: string;
  ip: string;
  ping?: number | null;
}

export interface VpnProfile {
  id: string;
  name: string;
  configPath: string;
  configContent: string;
  hosts: SplitRouteHost[];
}
