// ScreenLink UI 入口：布局、设备轮询、视图路由
import { listen } from "@tauri-apps/api/event";
import { api, displayName, type DeviceInfo, type JobSnapshot } from "./tauri";
import { el, store, subscribe, notify, toast } from "./store";
import { iconEl } from "./icons";
import { MirrorView } from "./mirror";
import { createTransferDrawer } from "./transfers";
import { createSettingsView } from "./settings";

const mirror = new MirrorView();
let transfersDrawer: ReturnType<typeof createTransferDrawer>;

type View = "onboarding" | "mirror" | "settings";
let currentView: View = "onboarding";

// ---------- 侧栏设备卡片 ----------
function deviceCard(d: DeviceInfo): HTMLElement {
  const online = d.state === "online";
  const transportChip = el("span", { class: `chip ${d.transport}` },
    d.transport === "usb" ? "USB" : d.transport === "wifi" ? "WiFi" : "模拟器");
  const stateChip = online ? null
    : el("span", { class: "chip off" }, d.state === "unauthorized" ? "未授权" : "离线");

  const mirroring = store.mirroringSerial === d.serial;
  const card = el("div", { class: `device-card${mirroring ? " active" : ""}` },
    el("div", { class: "row1" },
      el("span", { class: "name" }, displayName(d)),
      transportChip,
      stateChip,
    ),
    el("div", { class: "meta" },
      el("span", {}, d.serial),
      d.battery != null ? el("span", { style: "display:inline-flex;align-items:center;gap:4px" }, iconEl("battery", 12), `${d.battery}%`) : null,
      d.android_version ? el("span", {}, `Android ${d.android_version}`) : null,
    ),
    el("div", { class: "actions" },
      el("button", {
        class: "primary small",
        disabled: !online,
        onclick: () => switchToMirror(d),
      }, mirroring ? "已投屏" : "投屏"),
      el("button", {
        class: "small",
        disabled: !online,
        onclick: async () => {
          const f = await pickFiles();
          if (f.length) {
            await api.pushFiles(d.serial, f, store.settings.push_dir, "rename");
            toast(`正在发送 ${f.length} 个文件`);
          }
        },
      }, "发送文件"),
      el("button", {
        class: "ghost small",
        disabled: !online,
        onclick: async () => {
          const p = prompt("输入手机上的文件路径（如 /sdcard/DCIM/photo.jpg）：");
          if (p) {
            await api.pullFile(d.serial, p, store.settings.pull_dir, "rename");
            transfersDrawer.show();
          }
        },
      }, "接收"),
    ),
  );
  return card;
}

async function pickFiles(): Promise<string[]> {
  const { open } = await import("@tauri-apps/plugin-dialog");
  const files = await open({ multiple: true, title: "选择要发送的文件" });
  if (!files) return [];
  return Array.isArray(files) ? files : [files];
}

async function switchToMirror(d: DeviceInfo) {
  if (store.mirroringSerial === d.serial) return;
  currentView = "mirror";
  renderMain();
  await mirror.open(d.serial);
  store.mirroringSerial = d.serial;
  notify();
  transfersDrawer.show();
}

// ---------- 侧栏 ----------
function renderSidebar() {
  const list = document.getElementById("deviceList")!;
  list.innerHTML = "";

  const online = store.devices.filter((d) => d.state === "online");
  const others = store.devices.filter((d) => d.state !== "online");

  const usbTitle = document.getElementById("usbTitle")!;
  usbTitle.textContent = `设备 (${online.length + others.length})`;

  if (store.devices.length === 0) {
    list.append(el("div", { class: "section-title", style: "padding-left:6px" }, "尚未检测到设备，插上 USB 线或使用无线连接"));
  }
  for (const d of online) list.append(deviceCard(d));
  for (const d of others) list.append(deviceCard(d));

  // AI 状态指示
  const aiDot = document.getElementById("aiStatus")!;
  aiDot.innerHTML = "";
        aiDot.append(
          iconEl("dot", 10),
          document.createTextNode(store.aiEnabled ? " AI 访问已开启" : " AI 访问关闭"),
        );
  aiDot.style.color = store.aiEnabled ? "var(--accent)" : "var(--text-faint)";
}

// ---------- 主区 ----------
const main = document.querySelector(".main")!;

function renderMain() {
  main.innerHTML = "";
  if (currentView === "mirror" && store.mirroringSerial) {
    const d = store.devices.find((x) => x.serial === store.mirroringSerial);
    main.append(mirror.toolbar(d ? displayName(d) : store.mirroringSerial));
    main.append(mirror.stage);
    main.append(transfersDrawer.el);
    mirror.canvas.focus();
  } else if (currentView === "settings") {
    main.append(createSettingsView());
  } else {
    main.append(createOnboarding());
  }
}

function createOnboarding(): HTMLElement {
  const scroll = el("div", { class: "panel-scroll" });
  const inner = el("div", { class: "panel-narrow" },
    el("h2", {}, "连接你的手机"),
    el("p", { class: "desc" }, "USB 一插即用；同一 WiFi 下可无线投屏与传输。"));
  inner.append(el("div", { class: "guide-grid" },
    el("div", { class: "guide-card" },
      el("h3", {}, "USB 连接 ", el("span", { class: "badge" }, "推荐")),
      el("ol", {},
        el("li", {}, "手机「设置 → 关于手机」连续点击「版本号」7 次，开启开发者模式"),
        el("li", {}, "「设置 → 开发者选项」打开「USB 调试」"),
        el("li", {}, "数据线连接电脑，手机弹窗选择「允许 USB 调试」"),
        el("li", {}, "左侧列表出现设备后，点击「投屏」"),
      ),
    ),
    el("div", { class: "guide-card" },
      el("h3", {}, "无线连接（安卓 11+）"),
      el("ol", {},
        el("li", {}, "「开发者选项 → 无线调试」开启"),
        el("li", {}, "点「使用配对码配对设备」，得到 IP:端口 与 6 位配对码"),
        el("li", {}, "左侧「无线连接」→「配对新设备」输入配对码"),
        el("li", {}, "配对成功后自动出现在设备列表"),
      ),
    ),
  ));
  inner.append(el("div", { style: "margin-top:14px;display:flex;gap:8px" },
    el("button", { onclick: () => switchView("settings") }, "打开设置与 AI 面板"),
    el("button", { class: "ghost", onclick: () => api.listDevices() }, "重新扫描"),
  ));
  scroll.append(inner);
  return scroll;
}

function switchView(v: View) {
  if (currentView === "mirror" && v !== "mirror") {
    // 保持投屏会话，仅隐藏视图
  }
  currentView = v;
  renderMain();
}

// ---------- 无线连接面板（侧栏内） ----------
function lanPanel(): HTMLElement {
  const wrap = el("div", {});
  const list = el("div", { class: "lan-list" });
  const scanning = el("div", { style: "display:none;padding:0 6px 8px;color:var(--text-faint);font-size:11.5px" }, "扫描中…");

  async function scan() {
    scanning.style.display = "block";
    list.innerHTML = "";
    try {
      const r = await api.scanLan();
      const items: HTMLElement[] = [];
      for (const svc of r.mdns) {
        const kind = svc.kind === "pairing" ? "待配对" : "可连接";
        items.push(el("div", { class: "lan-item" },
          el("span", { class: `chip ${svc.kind === "pairing" ? "warn" : "wifi"}` }, kind),
          el("span", { class: "nm" }, svc.name.split(".").slice(0, 2).join(".")),
          el("span", { class: "addr" }, svc.addr),
          svc.kind === "connect"
            ? el("button", {
              class: "primary small",
              onclick: async () => {
                try {
                  await api.connectAddr(svc.addr);
                  toast("已连接", svc.addr);
                  await api.listDevices();
                } catch (e) { toast("连接失败", String(e), true); }
              },
            }, "连接")
            : el("button", { class: "small", onclick: () => pairModal(svc.addr) }, "配对"),
        ));
      }
      for (const addr of r.scanned_5555) {
        items.push(el("div", { class: "lan-item" },
          el("span", { class: "chip wifi" }, "5555"),
          el("span", { class: "nm" }, "经典 adb tcpip 设备"),
          el("span", { class: "addr" }, addr),
          el("button", {
            class: "primary small",
            onclick: async () => {
              try { await api.connectAddr(addr); toast("已连接", addr); await api.listDevices(); }
              catch (e) { toast("连接失败", String(e), true); }
            },
          }, "连接"),
        ));
      }
      if (!items.length) {
        items.push(el("div", { style: "padding:6px;color:var(--text-faint);font-size:11.5px" },
          "未发现设备。确认手机「无线调试」已开启，或手动输入 IP 连接。"));
      }
      list.append(...items);
    } finally {
      scanning.style.display = "none";
    }
  }

  const pairModal = (presetAddr = "") => {
    const mask = el("div", { class: "modal-mask" });
    const addrInput = el("input", { type: "text", placeholder: "配对地址 IP:端口", value: presetAddr }) as HTMLInputElement;
    const codeInput = el("input", { type: "text", placeholder: "6 位配对码" }) as HTMLInputElement;
    const errBox = el("div", { class: "error" });
    const modal = el("div", { class: "modal" },
      el("h3", {}, "无线配对"),
      el("div", { class: "desc" }, "手机「无线调试 → 使用配对码配对设备」中获取地址与配对码"),
      el("div", { class: "field" }, el("label", {}, "配对地址"), addrInput),
      el("div", { class: "field" }, el("label", {}, "配对码"), codeInput),
      errBox,
      el("div", { class: "btns" },
        el("button", { onclick: () => mask.remove() }, "取消"),
        el("button", {
          class: "primary",
          onclick: async () => {
            errBox.textContent = "";
            try {
              const r = await api.pairAddr(addrInput.value.trim(), codeInput.value.trim()) as { message?: string };
              mask.remove();
              toast("配对成功", r?.message ?? "");
              await api.listDevices();
            } catch (e) {
              errBox.textContent = String(e).replace("error: ", "");
            }
          },
        }, "配对"),
      ),
    );
    mask.append(modal);
    document.body.append(mask);
  };

  const manualModal = () => {
    const mask = el("div", { class: "modal-mask" });
    const addrInput = el("input", { type: "text", placeholder: "IP:端口（默认 5555）" }) as HTMLInputElement;
    const errBox = el("div", { class: "error" });
    const modal = el("div", { class: "modal" },
      el("h3", {}, "手动连接"),
      el("div", { class: "desc" }, "已配对过的设备可直接输入 IP 连接（端口默认 5555，无线调试端口随机）"),
      el("div", { class: "field" }, el("label", {}, "设备地址"), addrInput),
      errBox,
      el("div", { class: "btns" },
        el("button", { onclick: () => mask.remove() }, "取消"),
        el("button", {
          class: "primary",
          onclick: async () => {
            try {
              await api.connectAddr(addrInput.value.trim());
              mask.remove();
              toast("已连接", addrInput.value);
              await api.listDevices();
            } catch (e) { errBox.textContent = String(e).replace("error: ", ""); }
          },
        }, "连接"),
      ),
    );
    mask.append(modal);
    document.body.append(mask);
  };

  const header = el("div", { class: "section-title" }, "无线连接",
    el("span", { class: "action", onclick: () => void scan() }, "扫描"));
  const btns = el("div", { style: "display:flex;gap:6px;padding:0 12px 10px" },
    el("button", { class: "small", onclick: () => pairModal() }, "配对新设备"),
    el("button", { class: "small", onclick: () => manualModal() }, "输入 IP"),
  );
  wrap.append(header, btns, scanning, list);
  return wrap;
}

// ---------- 启动 ----------
async function boot() {
  transfersDrawer = createTransferDrawer(() => {
    currentView = "mirror";
    if (store.mirroringSerial) renderMain();
  });

  // 顶栏 tabs（放主区上方工具条：设置入口）
  const settingsBtn = el("button", { class: "ghost", onclick: () => switchView("settings") }, iconEl("settings", 13), document.createTextNode(" 设置 / AI"));
  document.querySelector(".brand")!.append(settingsBtn);

  // 侧栏
  document.getElementById("sidebarDevices")!.append(lanPanel());

  // 初始设置
  try {
    const s = await api.getSettings();
    store.settings = s.settings;
    store.wsPort = s.wsPort;
    store.aiEnabled = s.aiEnabled;
  } catch { /* ignore */ }

  // 事件监听
  await listen<DeviceInfo[]>("devices", (ev) => {
    store.devices = ev.payload;
    renderSidebar();
  });
  await listen<string>("mirror-ended", () => {
    store.mirroringSerial = null;
    notify();
  });
  await listen<JobSnapshot[]>("transfers", (ev) => {
    transfersDrawer.update(ev.payload);
  });

  await api.listDevices();
  renderSidebar();
  renderMain();

  subscribe(() => renderSidebar());
}

document.addEventListener("DOMContentLoaded", () => {
  void boot();
});
