// 中间画布区：空状态（引导卡片）/ 连接中（loading + 步骤）/ 投屏画布 + 悬浮工具条
import { el, selectedDevice, store } from "../store";
import { iconEl } from "../icons";
import { MirrorSession, type SessionCallbacks } from "../session";

export class ScreenCanvas {
  area = el("div", { class: "canvas-area" });
  canvas = el("canvas", { id: "mirrorCanvas", tabindex: "0" });
  statsEl = el("div", { class: "canvas-stats hidden" });
  private overlay = el("div", { class: "drop-overlay" },
    el("div", { class: "box" }, "松手发送到手机",
      el("small", {}, `目标：${store.settings.push_dir}`)));
  session: MirrorSession;

  constructor(cb: SessionCallbacks) {
    this.session = new MirrorSession(this.canvas, {
      ...cb,
      onStreaming: () => {
        this.statsEl.classList.remove("hidden");
        this.render();
        cb.onStreaming();
      },
    });
    this.area.append(this.canvas, this.statsEl, this.overlay);
    this.render();
  }

  /** 依据 store.conn.phase 重渲染 */
  render() {
    const phase = store.conn.phase;
    // 清掉非 canvas/overlay/stats 的子节点（空状态/加载中）
    for (const child of [...this.area.children]) {
      if (child !== this.canvas && child !== this.statsEl && child !== this.overlay) child.remove();
    }

    if (phase === "idle") {
      this.statsEl.classList.add("hidden");
      this.canvas.classList.add("hidden");
      const d = selectedDevice();
      const inner = el("div", { class: "empty" },
        el("h2", {}, "选择设备并连接"),
        el("p", { class: "lead" },
          d ? `已选择「${deviceName(d)}」，点击顶部「连接」开始投屏` : "在左侧选择设备，或按以下步骤准备"),
        el("div", { class: "guide-grid" },
          guideCard("cable", "USB 连接", "推荐", [
            "手机「设置 → 关于手机」连点版本号 7 次",
            "开发者选项中打开「USB 调试」",
            "数据线连接，手机弹窗允许调试",
          ]),
          guideCard("wifi", "无线连接（安卓 11+）", "", [
            "开发者选项中开启「无线调试」",
            "使用配对码完成首次配对",
            "左侧「无线 → 扫描」发现并连接",
          ]),
        ),
      );
      this.area.append(inner);
      return;
    }

    if (phase === "connecting") {
      this.statsEl.classList.add("hidden");
      this.canvas.classList.add("hidden");
      this.area.append(el("div", { class: "connecting" },
        el("div", { class: "spinner" }),
        el("div", { class: "step" }, store.conn.step || "连接中…"),
      ));
      return;
    }

    // connected：悬浮工具条
    this.canvas.classList.remove("hidden");
    this.canvas.focus();
    const toolbar = el("div", { class: "canvas-toolbar" },
      tbBtn("back", "返回", () => this.session.sendKey("back")),
      tbBtn("home", "Home", () => this.session.sendKey("home")),
      tbBtn("recents", "后台", () => this.session.sendKey("recents")),
      tbBtn("camera", "截图", () => void this.session.screenshot()),
      tbBtn("maximize", "全屏", () => this.session.toggleFullscreen(this.area)),
    );
    this.area.append(toolbar);
  }

  /** MCP 截图请求：上报当前帧 */
  uploadFrame(serial: string) {
    void this.session.uploadFrame(serial);
  }
}

function tbBtn(icon: string, title: string, onClick: () => void): HTMLElement {
  return el("button", { class: "small", title, onclick: onClick }, iconEl(icon, 13),
    document.createTextNode(title));
}

function guideCard(icon: string, title: string, badge: string, steps: string[]): HTMLElement {
  return el("div", { class: "guide-card" },
    el("h3", {}, iconEl(icon, 14), document.createTextNode(title),
      badge ? el("span", { class: "badge" }, badge) : null),
    el("ol", {}, ...steps.map((s) => el("li", {}, s))),
  );
}

function deviceName(d: { manufacturer: string; model: string; serial: string }): string {
  const brand = d.manufacturer ? d.manufacturer[0].toUpperCase() + d.manufacturer.slice(1) : "";
  return brand && d.model ? `${brand} ${d.model}` : d.model || d.serial;
}
