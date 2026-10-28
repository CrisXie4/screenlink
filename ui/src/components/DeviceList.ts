// 左侧设备栏（240px，可折叠）：设备列表 + 选中高亮 + 无线连接操作 + USB 驱动修复
import { api, displayName, type DeviceInfo, type DiscoveryResult, type UsbStatus } from "../tauri";
import { el, selectedDevice, store, toast } from "../store";
import { iconEl } from "../icons";

export interface DeviceListHandlers {
  onSelect(serial: string): void;
  onConnect(serial: string): void;
}

export function createDeviceBar(h: DeviceListHandlers) {
  const list = el("div", { class: "scroll" });
  const lanBox = el("div", {});
  const bar = el("div", { class: "devicebar" });

  // ---- USB 驱动状态（老电脑：ADB 接口缺驱动时 adb 完全看不到设备） ----
  let usbStatus: UsbStatus | null = null;
  let usbCheckedAt = 0;
  let usbInstalling = false;

  async function refreshUsbStatus() {
    const now = Date.now();
    if (now - usbCheckedAt < 5000) return;
    usbCheckedAt = now;
    try {
      usbStatus = await api.usbDriverStatus();
    } catch {
      return; // 非 Windows 或枚举失败：静默
    }
    render();
  }

  async function installDriver() {
    if (usbInstalling) return;
    usbInstalling = true;
    render();
    try {
      const r = await api.usbDriverInstall();
      toast("USB 驱动已安装", r?.message ?? "重新插拔数据线后即可识别");
      usbCheckedAt = 0;
      await api.listDevices();
    } catch (e) {
      toast("驱动安装失败", String(e).replace("error: ", ""), true);
    } finally {
      usbInstalling = false;
      render();
    }
  }

  function usbHint(): HTMLElement | null {
    const hasOnlineUsb = store.devices.some((d) => d.state === "online" && d.transport === "usb");
    if (!usbStatus?.missing_driver || hasOnlineUsb) return null;
    const names = usbStatus.devices
      .filter((d) => d.status !== "OK")
      .map((d) => d.name)
      .slice(0, 2)
      .join("、") || "Android 设备";
    return el("div", { class: "guide", style: "color:var(--warn,#b45309)" },
      iconEl("cable", 12), ` 检测到「${names}」的 USB 驱动异常`, el("br"),
      el("span", { style: "color:var(--text-3)" }, "老系统常见问题，安装 Google USB 驱动可修复（需管理员确认）"),
      el("br"),
      el("button", {
        class: "primary small",
        style: "margin-top:6px",
        disabled: usbInstalling,
        onclick: () => void installDriver(),
      }, usbInstalling ? "正在下载并安装…" : "一键安装驱动"));
  }

  // ---- 渲染 ----
  function render() {
    list.innerHTML = "";
    // 连接中：高亮跟随已连接设备；空闲：跟随手动选中项
    const sel = store.conn.phase !== "idle" && store.conn.serial ? store.conn.serial : store.selected;
    const online = store.devices.filter((d) => d.state === "online");
    const others = store.devices.filter((d) => d.state !== "online");

    if (!store.devices.length) {
      list.append(
        el("div", { class: "guide" },
          "尚未检测到设备", el("br"),
          "1. 手机开启 USB 调试并用数据线连接", el("br"),
          "2. 或使用右侧「无线连接」"),
      );
    }
    const hint = usbHint();
    if (hint) list.append(hint);
    for (const d of [...online, ...others]) list.append(card(d, sel === d.serial));
    void refreshUsbStatus();

    // 无线发现结果
    lanBox.innerHTML = "";
    if (lanResults) {
      // 伴侣 App 信标（UDP 广播发现，无需 adb）：标记 + 配对/连接入口
      const paired = pairedPhones;
      for (const u of lanResults.udp) {
        const isPaired = paired.some((p) => p.ip.includes(u.ip));
        lanBox.append(lanItem(
          "App",
          u.name || "伴侣App 设备",
          `${u.ip}:${u.port}`,
          isPaired
            ? async () => {
                try {
                  const r = await api.phoneConnect(u.ip);
                  toast("已连接", `${r.name}（伴侣 App 链路就绪）`);
                } catch (e) {
                  toast("连接失败", String(e), true);
                }
              }
            : () => appPairModal(u.ip),
          isPaired ? "连接" : "配对",
        ));
      }
      for (const svc of lanResults.mdns) {
        lanBox.append(lanItem(
          svc.kind === "pairing" ? "待配对" : "可连接",
          svc.name.split(".").slice(0, 2).join("."),
          svc.addr,
          svc.kind === "pairing"
            ? () => pairModal(svc.addr)
            : async () => {
              try {
                await api.connectAddr(svc.addr);
                toast("已连接", svc.addr);
                await api.listDevices();
              } catch (e) {
                toast("连接失败", String(e), true);
              }
            },
          svc.kind === "pairing" ? "配对" : "连接",
        ));
      }
      for (const addr of lanResults.scanned_5555) {
        lanBox.append(lanItem("5555", "adb tcpip 设备", addr, async () => {
          try {
            await api.connectAddr(addr);
            toast("已连接", addr);
            await api.listDevices();
          } catch (e) {
            toast("连接失败", String(e), true);
          }
        }, "连接"));
      }
      if (!lanResults.mdns.length && !lanResults.scanned_5555.length) {
        lanBox.append(el("div", { class: "lan-item", style: "color:var(--text-3)" },
          "未发现设备，确认手机「无线调试」已开启"));
      }
    }
  }

  function card(d: DeviceInfo, selected: boolean): HTMLElement {
    const online = d.state === "online";
    // 无线调试 mDNS serial 过长且无意义：显示友好文案
    const meta1 = d.serial.includes("_tcp")
      ? "无线调试"
      : d.serial;
    const c = el("div", {
      class: `device-card${selected ? " selected" : ""}`,
      onclick: () => h.onSelect(d.serial),
      ondblclick: () => online && h.onConnect(d.serial),
    },
      el("div", { class: "row1" },
        el("span", { class: "nm" }, displayName(d)),
        el("span", { style: "display:flex;gap:4px" },
          d.has_app ? el("span", { class: "chip app" }, "App") : null,
          el("span", { class: `chip ${d.transport === "wifi" ? "wifi" : d.transport === "usb" ? "usb" : ""}` },
            iconEl(d.transport === "wifi" ? "wifi" : "cable", 11),
            document.createTextNode(d.transport === "wifi" ? "WiFi" : d.transport === "usb" ? "USB" : "模拟器"),
          ),
        ),
      ),
      el("div", { class: "meta" },
        el("span", {}, meta1),
        d.battery != null ? el("span", { style: "display:inline-flex;align-items:center;gap:3px" }, iconEl("battery", 11), `${d.battery}%`) : null,
        online ? null : el("span", { class: "chip off" }, d.state === "unauthorized" ? "未授权" : "离线"),
      ),
    );
    return c;
  }

  function lanItem(kind: string, name: string, addr: string, onGo: () => void, goLabel: string): HTMLElement {
    const btn = el("button", { class: "primary small", onclick: onGo }, goLabel);
    const chipClass = kind === "待配对" ? "off" : kind === "App" ? "app" : "wifi";
    return el("div", { class: "lan-item" },
      el("span", { class: `chip ${chipClass}` }, kind),
      el("span", { class: "nm" }, name),
      el("span", { class: "addr" }, addr),
      btn,
    );
  }

  // ---- 无线操作 ----
  let lanResults: DiscoveryResult | null = null;
  let pairedPhones: { tokenId: string; name: string; ip: string }[] = [];
  const scanHint = el("div", { class: "lan-item", style: "display:none;color:var(--text-3)" }, "扫描中…");

  async function refreshPaired() {
    try {
      pairedPhones = (await api.phonePaired()).phones;
    } catch {
      pairedPhones = [];
    }
  }

  /// 伴侣 App 配对弹窗（两步）：获取配对码（手机端显示）→ 在此输入并提交。
  function appPairModal(presetIp = "") {
    const ipInput = el("input", { type: "text", placeholder: "手机 IP（端口自动 27188）", value: presetIp }) as HTMLInputElement;
    const codeInput = el("input", { type: "text", placeholder: "6 位配对码", style: "display:none" }) as HTMLInputElement;
    const stepHint = el("div", { class: "desc" },
      "确保手机上的 ScreenLink 伴侣 App 服务已开启，且与电脑同一局域网");
    const errBox = el("div", { class: "error" });
    let phase: "idle" | "waiting" = "idle";

    const getCodeBtn = el("button", {
      class: "primary",
      onclick: async () => {
        errBox.textContent = "";
        const ip = ipInput.value.trim();
        if (!ip) { errBox.textContent = "请输入手机 IP"; return; }
        getCodeBtn.disabled = true;
        getCodeBtn.textContent = "连接中…";
        try {
          await api.phonePairBegin(ip);
          phase = "waiting";
          stepHint.textContent = "手机上已显示 6 位配对码（App 通知栏 / 状态页），请在下方输入（5 分钟内有效）";
          codeInput.style.display = "";
          codeInput.focus();
          getCodeBtn.style.display = "none";
          submitBtn.style.display = "";
          cancelBtn.textContent = "取消配对";
        } catch (e) {
          errBox.textContent = String(e).replace("error: ", "");
          getCodeBtn.disabled = false;
          getCodeBtn.textContent = "获取配对码";
        }
      },
    }, "获取配对码");
    const submitBtn = el("button", {
      class: "primary",
      style: "display:none",
      onclick: async () => {
        errBox.textContent = "";
        const code = codeInput.value.trim();
        if (code.length !== 6) { errBox.textContent = "请输入 6 位配对码"; return; }
        submitBtn.disabled = true;
        try {
          const r = await api.phonePairSubmit(code);
          mask.remove();
          toast("配对成功", `${r.name} 已信任此电脑`);
          await refreshPaired();
          await api.listDevices();
        } catch (e) {
          errBox.textContent = String(e).replace("error: ", "");
          submitBtn.disabled = false;
        }
      },
    }, "配对");
    const cancelBtn = el("button", {
      onclick: () => {
        void api.phonePairCancel().catch(() => {});
        mask.remove();
      },
    }, "取消");

    const mask = el("div", { class: "modal-mask" }, el("div", { class: "modal" },
      el("h3", {}, "配对伴侣 App"),
      el("div", { class: "field" },
        el("div", {}, el("div", {}, "手机 IP"),
          el("div", { class: "hint" }, "手机与电脑同一局域网；可在上方扫描结果的 App 条目中查看 IP")),
        ipInput,
      ),
      stepHint,
      codeInput,
      errBox,
      el("div", { class: "btns" },
        cancelBtn,
        getCodeBtn,
        submitBtn,
      ),
    ));
    mask.addEventListener("click", (e) => e.target === mask && (void api.phonePairCancel().catch(() => {}), mask.remove()));
    document.body.append(mask);
    ipInput.focus();
    void phase;
  }

  async function scan() {
    scanHint.style.display = "block";
    try {
      lanResults = await api.scanLan();
      await refreshPaired();
      render();
    } catch (e) {
      toast("扫描失败", String(e), true);
    } finally {
      scanHint.style.display = "none";
    }
  }

  function field(label: string, control: HTMLElement, hint = ""): HTMLElement {
    return el("div", { class: "field" },
      el("div", {},
        el("div", {}, label),
        hint ? el("div", { class: "hint" }, hint) : null,
      ),
      control,
    );
  }

  function switchInput(checked: boolean): HTMLInputElement {
    const input = el("input", { type: "checkbox" }) as HTMLInputElement;
    input.checked = checked;
    return el("label", { class: "switch" }, input, el("span", { class: "track" })).querySelector("input")!;
  }

  function pairModal(presetAddr = "") {
    const addrInput = el("input", { type: "text", placeholder: "IP:端口", value: presetAddr }) as HTMLInputElement;
    const codeInput = el("input", { type: "text", placeholder: "6 位配对码" }) as HTMLInputElement;
    const errBox = el("div", { class: "error" });
    const mask = el("div", { class: "modal-mask" }, el("div", { class: "modal" },
      el("h3", {}, "无线配对"),
      el("div", { class: "desc" }, "手机「无线调试 → 使用配对码配对设备」中获取地址与配对码"),
      field("配对地址", addrInput),
      field("配对码", codeInput),
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
    ));
    mask.addEventListener("click", (e) => e.target === mask && mask.remove());
    document.body.append(mask);
    addrInput.focus();
  }

  function manualModal() {
    const addrInput = el("input", { type: "text", placeholder: "IP:端口（默认 5555）" }) as HTMLInputElement;
    const errBox = el("div", { class: "error" });
    const mask = el("div", { class: "modal-mask" }, el("div", { class: "modal" },
      el("h3", {}, "手动连接"),
      el("div", { class: "desc" }, "已配对过的设备直接输入地址连接（无线调试端口随机，见手机设置）"),
      field("设备地址", addrInput),
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
            } catch (e) {
              errBox.textContent = String(e).replace("error: ", "");
            }
          },
        }, "连接"),
      ),
    ));
    mask.addEventListener("click", (e) => e.target === mask && mask.remove());
    document.body.append(mask);
    addrInput.focus();
  }

  // ---- 组装 ----
  const lanToggleBtn = el("button", { class: "ghost small", onclick: () => lanWrap.classList.toggle("hidden") },
    iconEl("wifi", 13), document.createTextNode("无线"));
  const lanWrap = el("div", { class: "hidden" },
    el("div", { class: "lan-ops" },
      el("button", { class: "small", onclick: () => void scan() }, iconEl("refresh", 12), "扫描"),
      el("button", { class: "small", onclick: () => pairModal() }, "配对"),
      el("button", { class: "small", onclick: () => manualModal() }, "输入 IP"),
    ),
    scanHint,
    lanBox,
  );

  bar.append(
    el("div", { class: "head" },
      el("span", { class: "t" }, "设备"),
      lanToggleBtn,
    ),
    list,
    lanWrap,
    el("div", { class: "foot" }, iconEl("phone", 12), document.createTextNode("连接后点「连接」开始投屏")),
  );

  return {
    el: bar,
    render,
  };
}
