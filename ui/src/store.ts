// 轻量应用状态 + DOM 工具
import type { DeviceInfo, Settings } from "./tauri";

export interface Store {
  devices: DeviceInfo[];
  mirroringSerial: string | null;
  settings: Settings;
  wsPort: number;
  aiEnabled: boolean;
}

export const store: Store = {
  devices: [],
  mirroringSerial: null,
  settings: {
    max_size: 1600,
    bit_rate: 12_000_000,
    max_fps: 60,
    push_dir: "/sdcard/Download/ScreenLink",
    pull_dir: "",
    clipboard_sync: true,
    auto_switch_wifi: true,
  },
  wsPort: 27193,
  aiEnabled: false,
};

type Listener = () => void;
const listeners = new Set<Listener>();
export function subscribe(fn: Listener) {
  listeners.add(fn);
  return () => listeners.delete(fn);
}
export function notify() {
  listeners.forEach((fn) => fn());
}

/** 创建带 class/子元素/事件的元素 */
export function el<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  attrs: Record<string, string | boolean | EventListenerOrEventListenerObject> = {},
  ...children: (Node | string | null | undefined)[]
): HTMLElementTagNameMap[K] {
  const node = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs)) {
    if (typeof v === "boolean") {
      if (v) node.setAttribute(k, "");
    } else if (k.startsWith("on")) {
      node.addEventListener(k.slice(2), v as EventListener);
    } else {
      node.setAttribute(k, v as string);
    }
  }
  for (const c of children) {
    if (c == null) continue;
    node.append(typeof c === "string" ? document.createTextNode(c) : c);
  }
  return node;
}

/** 右下角 toast */
export function toast(title: string, body = "", isError = false) {
  const box = document.querySelector(".toasts")!;
  const t = el("div", { class: `toast${isError ? " error" : ""}` },
    el("div", { class: "t-title" }, title),
    body ? el("div", { class: "t-body" }, body) : null);
  box.append(t);
  setTimeout(() => t.remove(), isError ? 6000 : 3500);
}

export function fmtBytes(n: number): string {
  const u = ["B", "KB", "MB", "GB", "TB"];
  let v = n;
  let i = 0;
  while (v >= 1024 && i < u.length - 1) { v /= 1024; i++; }
  return `${v.toFixed(1)} ${u[i]}`;
}

export function fmtSpeed(bps: number): string {
  return `${fmtBytes(bps)}/s`;
}
