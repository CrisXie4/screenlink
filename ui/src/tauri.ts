// Tauri 后端调用的类型化封装
import { invoke } from "@tauri-apps/api/core";

export interface DeviceInfo {
  serial: string;
  state: "online" | "offline" | "unauthorized";
  transport: "usb" | "wifi" | "emulator";
  model: string;
  manufacturer: string;
  android_version: string;
  sdk: string;
  battery?: number | null;
  wifi_ip?: string | null;
  mirroring: boolean;
}

export function displayName(d: DeviceInfo): string {
  const brand = d.manufacturer ? d.manufacturer[0].toUpperCase() + d.manufacturer.slice(1) : "";
  return brand && d.model ? `${brand} ${d.model}` : d.model || d.serial;
}

export interface MdnsService {
  name: string;
  service_type: string;
  addr: string;
  kind: "connect" | "pairing";
}

export interface DiscoveryResult {
  mdns: MdnsService[];
  udp: { name: string; serial: string; ip: string; port: number }[];
  scanned_5555: string[];
}

export interface JobSnapshot {
  id: number;
  kind: "push" | "pull";
  serial: string;
  title: string;
  state: "queued" | "running" | "done" | "failed" | "cancelled";
  total_bytes: number;
  copied_bytes: number;
  percent: number;
  speed_bps: number;
  eta_secs: number;
  error?: string | null;
  file_count: number;
}

export interface Settings {
  max_size: number;
  bit_rate: number;
  max_fps: number;
  push_dir: string;
  pull_dir: string;
  clipboard_sync: boolean;
  auto_switch_wifi: boolean;
}

export interface AuditEntry {
  ts: string;
  client: string;
  tool: string;
  device: string;
  args: unknown;
  ok: boolean;
  detail?: string;
  took_ms: number;
}

const call = <T = unknown>(cmd: string, args?: Record<string, unknown>) =>
  invoke<T>(cmd, args);

export const api = {
  listDevices: () => call<{ devices: DeviceInfo[]; mirroring: string[] }>("list_devices"),
  connectAddr: (addr: string) => call("connect_addr", { addr }),
  disconnectAddr: (addr: string) => call("disconnect_addr", { addr }),
  pairAddr: (addr: string, code: string) => call("pair_addr", { addr, code }),
  enableTcpip: (serial: string) => call<{ ip: string | null; addr: string | null }>("enable_tcpip", { serial }),
  scanLan: () => call<DiscoveryResult>("scan_lan"),
  startMirror: (serial: string) =>
    call<{ info: { width: number; height: number }; wsPort: number }>("start_mirror", { serial }),
  stopMirror: (serial: string) => call("stop_mirror", { serial }),
  sendTouch: (serial: string, action: number, x: number, y: number, w: number, h: number, actionButton = 1, buttons = 1) =>
    call("send_touch", { serial, action, x, y, w, h, actionButton, buttons }),
  sendScroll: (serial: string, x: number, y: number, w: number, h: number, dy: number) =>
    call("send_scroll", { serial, x, y, w, h, dy }),
  sendText: (serial: string, text: string) => call("send_text", { serial, text }),
  sendKey: (serial: string, key: string) => call("send_key", { serial, key }),
  deviceClipboard: (serial: string, action: "set" | "get", text?: string) =>
    call("device_clipboard", { serial, action, text: text ?? null }),
  pushFiles: (serial: string, paths: string[], remoteDir: string, policy?: string) =>
    call<{ jobId: number }>("push_files", { serial, paths, remoteDir, policy: policy ?? null }),
  pullFile: (serial: string, remotePath: string, localDir: string, policy?: string) =>
    call<{ jobId: number }>("pull_file", { serial, remotePath, localDir, policy: policy ?? null }),
  listTransfers: () => call<JobSnapshot[]>("list_transfers"),
  cancelTransfer: (id: number) => call("cancel_transfer", { id }),
  uploadFrame: (serial: string, mime: string, dataB64: string) =>
    call("upload_frame", { serial, mime, dataB64 }),
  getSettings: () => call<{ settings: Settings; aiEnabled: boolean; wsPort: number }>("get_settings"),
  setSettings: (settings: Settings) => call("set_settings", { settings }),
  setClipboardSync: (on: boolean) => call("set_clipboard_sync", { on }),
  enableAi: () => call<{ token: string; snippet: string; scopes: string[] }>("enable_ai"),
  disableAi: () => call("disable_ai"),
  getAiInfo: () => call<{ enabled: boolean; snippet: string }>("get_ai_info"),
  getAudit: (limit = 100) => call<{ entries: AuditEntry[] }>("get_audit", { limit }),
  clearAudit: () => call("clear_audit"),
  writeScreenshot: (serial: string, dataB64: string) => call("write_screenshot", { serial, dataB64 }),
};
