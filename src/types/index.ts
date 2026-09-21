/**
 * A server entry. What the user types into its `host` is also the network
 * swept on refresh: `192.168.101.1` means the /24 that address sits in.
 */
export interface AdbServer {
  id: string;
  host: string;
  port: number;
  enabled: boolean;
}

/** Progress of a wireless-adb sweep, sent while the refresh button runs one. */
export interface ScanProgress {
  /** The daemon the sweep attaches through — the local one, always. */
  daemonHost: string;
  daemonPort: number;
  segment: string;
  scanned: number;
  /** Addresses this sweep probes: the ones already connected are not probed. */
  total: number;
  /** Addresses that answered, as `ip:5555` — the newly found ones. */
  found: string[];
  connected: number;
  /** Devices in the segment that were attached before the sweep ran. */
  alreadyConnected: number;
  done: boolean;
  error?: string | null;
}

export type DeviceStatus = 'online' | 'offline' | 'unauthorized' | 'connecting';

export interface Device {
  serial: string;
  status: DeviceStatus;
  model: string;
  battery: number;
  screen_width: number;
  screen_height: number;
  server_host: string;
  server_port: number;
}

export interface CommandResult {
  serial: string;
  success: boolean;
  message: string;
}

export interface DeviceResolution {
  serial: string;
  width: number;
  height: number;
  server_host: string;
  server_port: number;
}

export interface AppConfig {
  servers: AdbServer[];
}
