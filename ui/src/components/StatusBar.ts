// 顶部状态栏：品牌 + 设备名 + 连接通道 + 电量 + 连接/断开 + 设置
import { el, selectedDevice, store, type Phase } from "../store";
import { iconEl } from "../icons";

export interface StatusBarHandlers {
  onConnect(): void;
  onDisconnect(): void;
  onToggleDeviceBar(): void;
  onOpenSettings(): void;
}

export function renderStatusBar(root: HTMLElement, h: StatusBarHandlers) {
  root.innerHTML = "";
  const d = selectedDevice();
  const phase: Phase = store.conn.phase;

  const brand = el(
    "div",
    { class: "brand" },
    el("span", { class: "logo" }, "S"),
    document.createTextNode("ScreenLink"),
  );

  const toggleBtn = el("button", {
    class: "ghost icon-only",
    title: "展开/收起设备栏",
    onclick: () => h.onToggleDeviceBar(),
  }, iconEl("panelLeft", 15));

  // 设备信息区
  const info = el("div", { class: "dev-info" });
  if (!d) {
    info.append(el("span", { class: "placeholder" }, "未选择设备"));
  } else {
    info.append(el("span", { class: "name" }, displayName(d)));
    const chip = el("span", { class: `chip ${d.transport === "wifi" ? "wifi" : d.transport === "usb" ? "usb" : ""}` },
      iconEl(d.transport === "wifi" ? "wifi" : "cable", 12),
      document.createTextNode(d.transport === "wifi" ? "WiFi" : d.transport === "usb" ? "USB" : "模拟器"),
    );
    info.append(chip);
    if (d.state !== "online") {
      info.append(el("span", { class: "chip off" }, d.state === "unauthorized" ? "未授权" : "离线"));
    } else if (d.battery != null) {
      info.append(el("span", { class: "chip" }, iconEl("battery", 12), document.createTextNode(`${d.battery}%`)));
    }
  }

  // 连接/断开按钮
  let action: HTMLElement;
  if (phase === "connected") {
    action = el("button", { class: "primary", onclick: () => h.onDisconnect() },
      iconEl("unplug", 13), document.createTextNode("断开连接"));
  } else if (phase === "connecting") {
    action = el("button", { disabled: true },
      iconEl("loader", 13), document.createTextNode("连接中…"));
  } else {
    action = el("button", {
      class: "primary",
      disabled: !d || d.state !== "online",
      onclick: () => h.onConnect(),
    }, iconEl("play", 13), document.createTextNode("连接"));
  }

  root.append(
    brand,
    toggleBtn,
    info,
    el("span", { class: "spacer" }),
    action,
    el("button", { class: "ghost icon-only", title: "设置 / AI", onclick: () => h.onOpenSettings() },
      iconEl("settings", 15)),
  );
}

function displayName(d: { manufacturer: string; model: string; serial: string }): string {
  const brand = d.manufacturer ? d.manufacturer[0].toUpperCase() + d.manufacturer.slice(1) : "";
  return brand && d.model ? `${brand} ${d.model}` : d.model || d.serial;
}
