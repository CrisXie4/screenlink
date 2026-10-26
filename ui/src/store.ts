// 应用状态（唯一数据源；组件订阅 notify 重渲染）
import type { DeviceInfo, Settings } from "./tauri";

export type Phase = "idle" | "connecting" | "connected";

export interface Store {
  devices: DeviceInfo[];
  /** 左侧列表中选中的设备（连接由此发起） */
  selected: string | null;
  conn: { phase: Phase; serial: string | null; step: string };
  settings: Settings;
  aiEnabled: boolean;
  wsPort: number;
}

export const store: Store = {
  devices: [],
  selected: null,
  conn: { phase: "idle", serial: null, step: "" },
  settings: {
    max_size: 1600,
    bit_rate: 12_000_000,
    max_fps: 60,
    keyframe_interval: 1,
    push_dir: "/sdcard/Download/ScreenLink",
    pull_dir: "",
    clipboard_sync: true,
    auto_switch_wifi: true,
  },
  aiEnabled: false,
  wsPort: 27193,
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

export function selectedDevice(): DeviceInfo | null {
  return store.devices.find((d) => d.serial === store.selected) ?? null;
}

/** 创建带 class/子元素/事件的元素（children 支持 Node/string/数组） */
export function el<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  attrs: Record<string, string | boolean | EventListenerOrEventListenerObject> = {},
  ...children: (Node | string | null | undefined | (Node | string)[])[]
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
  append(node, children);
  return node;
}

function append(node: HTMLElement, children: (Node | string | null | undefined | (Node | string)[])[]) {
  for (const c of children) {
    if (c == null) continue;
    if (Array.isArray(c)) {
      append(node, c);
    } else {
      node.append(typeof c === "string" ? document.createTextNode(c) : c);
    }
  }
}

/** 右上角 toast（错误统一走 toast，不弹窗） */
export function toast(title: string, body = "", isError = false) {
  const box = document.querySelector(".toasts")!;
  const t = el(
    "div",
    { class: `toast${isError ? " error" : ""}` },
    el("div", { class: "t-title" }, title),
    body ? el("div", { class: "t-body" }, body) : null,
  );
  box.append(t);
  setTimeout(() => t.remove(), isError ? 6000 : 3200);
}

export function fmtBytes(n: number): string {
  const u = ["B", "KB", "MB", "GB", "TB"];
  let v = n;
  let i = 0;
  while (v >= 1024 && i < u.length - 1) {
    v /= 1024;
    i++;
  }
  return `${v.toFixed(1)} ${u[i]}`;
}

export function fmtSpeed(bps: number): string {
  return `${fmtBytes(bps)}/s`;
}
