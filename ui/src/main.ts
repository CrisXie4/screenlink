// ScreenLink 入口：组装 StatusBar / DeviceBar / ScreenCanvas / TransferQueue
import { listen } from "@tauri-apps/api/event";
import { api, type DeviceInfo, type JobSnapshot } from "./tauri";
import { el, notify, store, subscribe, toast } from "./store";
import { renderStatusBar } from "./components/StatusBar";
import { createDeviceBar } from "./components/DeviceList";
import { ScreenCanvas } from "./components/ScreenCanvas";
import { TransferQueue } from "./components/TransferQueue";
import { openSettingsDialog } from "./components/SettingsDialog";

// ---- 组件 ----
const statusRoot = el("div", { class: "statusbar" });
const canvas = new ScreenCanvas({
  onStep: (step) => {
    store.conn.step = step;
    notify();
  },
  onStreaming: () => {
    if (store.conn.phase !== "connected") {
      store.conn.phase = "connected";
      store.conn.step = "";
      notify();
    }
  },
  onClosed: (reason) => {
    if (store.conn.phase === "connected" || store.conn.phase === "connecting") {
      store.conn = { phase: "idle", serial: null, step: "" };
      if (reason) toast("投屏已断开", reason, true);
      notify();
    }
  },
  onStats: (s) => {
    canvasEl.statsEl.textContent =
      `${s.fps.toFixed(0)} fps · ${s.width}x${s.height}${s.latencyMs > 1 ? ` · ${s.latencyMs.toFixed(0)}ms` : ""}`;
  },
  onDrop: (paths) => void canvasEl.session.pushDropped(paths),
});
const canvasEl = canvas;

const deviceBar = createDeviceBar({
  onSelect: (serial) => {
    store.selected = serial;
    notify();
  },
  onConnect: (serial) => connectDevice(serial),
});

const queue = new TransferQueue();

// ---- 连接控制 ----
let connecting = false;

async function connectDevice(serial: string) {
  const d = store.devices.find((x) => x.serial === serial);
  if (!d || d.state !== "online" || connecting) return;
  connecting = true;
  store.selected = serial;
  store.conn = { phase: "connecting", serial, step: "正在启动服务…" };
  notify();
  await canvas.session.connect(serial);
  connecting = false;
}

function disconnect() {
  canvas.session.disconnect();
  store.conn = { phase: "idle", serial: null, step: "" };
  notify();
}

function renderStatusBarNow() {
  renderStatusBar(statusRoot, {
    onConnect: () => store.selected && void connectDevice(store.selected),
    onDisconnect: disconnect,
    onToggleDeviceBar: () => deviceBar.el.classList.toggle("collapsed"),
    onOpenSettings: () => openSettingsDialog(() => {}),
  });
}

// ---- 事件与启动 ----
async function boot() {
  document.getElementById("app")!.append(
    statusRoot,
    el("div", { class: "body" }, deviceBar.el,
      el("div", { class: "main" }, canvas.area, queue.el)),
  );

  await listen<DeviceInfo[]>("devices", (ev) => {
    store.devices = ev.payload;
    deviceBar.render();
    notify();
  });
  await listen<string>("mirror-ended", () => {
    if (store.conn.phase !== "idle") {
      store.conn = { phase: "idle", serial: null, step: "" };
      notify();
    }
  });
  await listen<JobSnapshot[]>("transfers", () => {
    /* TransferQueue 自行监听 */
  });
  // MCP 快速截图：核心向 webview 请求当前画布帧
  await listen<{ serial: string }>("frame-request", (ev) => {
    canvas.uploadFrame(ev.payload.serial);
  });

  try {
    const s = await api.getSettings();
    store.settings = s.settings;
    store.wsPort = s.wsPort;
    store.aiEnabled = s.aiEnabled;
  } catch {
    /* ignore */
  }

  const devices = await api.listDevices();
  store.devices = devices.devices;
  if (devices.mirroring.length) store.selected = devices.mirroring[0];
  deviceBar.render();
  renderStatusBarNow();
  notify();

  subscribe(() => renderStatusBarNow());
}

document.addEventListener("DOMContentLoaded", () => {
  void boot();
});
