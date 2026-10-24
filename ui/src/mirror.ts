// 投屏组件：WebCodecs 硬解 + Canvas 渲染 + 电脑反控 + 拖拽传文件
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { api, displayName } from "./tauri";
import { el, fmtBytes, store, toast } from "./store";
import { iconEl } from "./icons";

interface VideoInfo { width: number; height: number; }

const KEYCODE_MAP: Record<string, string> = {
  Enter: "enter", Backspace: "backspace", Escape: "back", Tab: "tab",
  ArrowUp: "up", ArrowDown: "down", ArrowLeft: "left", ArrowRight: "right",
  Home: "home_key", End: "move_end", PageUp: "page_up", PageDown: "page_down",
};

export class MirrorView {
  stage = el("div", { class: "mirror-stage hidden" });
  canvas = el("canvas", { id: "mirrorCanvas", tabindex: "0" });
  hud = el("div", { class: "hud hidden" });
  overlay = el("div", { class: "drop-overlay" },
    el("div", { class: "box" }, "松手发送到手机",
      el("small", {}, "文件将传到手机 Download/ScreenLink")));
  drawer?: HTMLElement;

  private serial: string | null = null;
  private info: VideoInfo = { width: 1080, height: 2400 };
  private decoder: VideoDecoder | null = null;
  private ws: WebSocket | null = null;
  private frameCount = 0;
  private fpsTime = 0;
  private latencyEMA = 0;
  private lastPts = 0;
  private pointerDown = false;
  private configureFailed = false;
  private pendingMove: { x: number; y: number } | null = null;
  private moveScheduled = false;
  private unlistenDrag: (() => void) | null = null;

  constructor() {
    this.stage.append(this.canvas, this.hud, this.overlay);
    this.bindInput();
    this.bindDragDrop();
  }

  async open(serial: string) {
    this.close();
    this.serial = serial;
    this.stage.classList.remove("hidden");
    try {
      const res = await api.startMirror(serial);
      this.info = res.info;
      store.wsPort = res.wsPort;
      this.connectWs();
      this.canvas.focus();
    } catch (e) {
      toast("投屏失败", String(e), true);
      this.close();
    }
  }

  close() {
    if (this.serial) {
      void api.stopMirror(this.serial).catch(() => {});
    }
    this.serial = null;
    this.ws?.close();
    this.ws = null;
    if (this.decoder) {
      try { this.decoder.close(); } catch { /* already closed */ }
      this.decoder = null;
    }
    this.stage.classList.add("hidden");
    this.hud.classList.add("hidden");
    this.unlistenDrag?.();
    this.unlistenDrag = null;
  }

  // ---- WebSocket 流接入 ----
  private connectWs() {
    if (!this.serial) return;
    const ws = new WebSocket(`ws://127.0.0.1:${store.wsPort}`);
    ws.binaryType = "arraybuffer";
    this.ws = ws;
    this.hud.classList.remove("hidden");
    this.hud.textContent = "WS 连接中…";
    ws.onopen = () => {
      ws.send(`mirror:${this.serial}`);
      this.hud.textContent = "WS 已连接，等待视频流…";
    };
    ws.onerror = () => {
      this.hud.textContent = "WS 错误";
      toast("WebSocket 错误", "无法连接视频流桥", true);
    };
    ws.onmessage = (ev) => {
      if (typeof ev.data === "string") {
        try {
          const j = JSON.parse(ev.data);
          if (j.info) { this.info = j.info; this.fitCanvas(); }
        } catch { /* ignore */ }
        return;
      }
      this.onChunk(new Uint8Array(ev.data));
    };
    ws.onclose = () => {
      if (this.serial) {
        // 会话结束（设备断开 / server 退出）
        toast("投屏已断开", "设备连接中断或会话结束");
        this.close();
      }
    };
  }

  private avcCDescription(configPayload: Uint8Array): { avcC: Uint8Array; codec: string } | null {
    // Annex-B config 包 -> avcC 盒（mp4 风格），并提取 profile 构造 codec 串
    const nals: Uint8Array[] = [];
    let i = 0;
    while (i < configPayload.length) {
      if (configPayload[i] === 0 && configPayload[i + 1] === 0 && configPayload[i + 2] === 1) {
        const nalStart = i + 3;
        let j = nalStart;
        while (j < configPayload.length - 2) {
          if (configPayload[j] === 0 && configPayload[j + 1] === 0 && (configPayload[j + 2] === 1 || configPayload[j + 3] === 1)) break;
          j++;
        }
        const end = j < configPayload.length - 2 ? (configPayload[j + 2] === 1 ? j : j + 1) : configPayload.length;
        nals.push(configPayload.subarray(nalStart, end));
        i = end;
      } else {
        i++;
      }
    }
    const sps = nals.find((n) => (n[0] & 0x1f) === 7);
    const pps = nals.find((n) => (n[0] & 0x1f) === 8);
    if (!sps || !pps) return null;
    const hex = (b: number) => b.toString(16).padStart(2, "0").toUpperCase();
    const codec = `avc1.${hex(sps[1])}${hex(sps[2])}${hex(sps[3])}`;
    // avcC: configurationVersion(1) profile compat level 0xFF | numOfSPS 0xE1 | len u16 sps | numOfPPS 1 | len u16 pps
    const out = new Uint8Array(11 + sps.length + pps.length);
    let o = 0;
    out[o++] = 1; out[o++] = sps[1]; out[o++] = sps[2]; out[o++] = sps[3];
    out[o++] = 0xff; out[o++] = 0xe1;
    out[o++] = (sps.length >> 8) & 0xff; out[o++] = sps.length & 0xff;
    out.set(sps, o); o += sps.length;
    out[o++] = 1;
    out[o++] = (pps.length >> 8) & 0xff; out[o++] = pps.length & 0xff;
    out.set(pps, o);
    return { avcC: out, codec };
  }

  /** Annex-B -> avcc（4 字节大端长度前缀） */
  private annexbToAvcc(payload: Uint8Array): Uint8Array {
    const nals: Uint8Array[] = [];
    let i = 0;
    const starts: number[] = [];
    while (i < payload.length) {
      if (payload[i] === 0 && payload[i + 1] === 0 && payload[i + 2] === 1) {
        starts.push(i + 3);
        i += 3;
      } else {
        i++;
      }
    }
    for (let k = 0; k < starts.length; k++) {
      const end = k + 1 < starts.length ? this.prevStart(starts, k + 1, payload) : payload.length;
      nals.push(payload.subarray(starts[k], end));
    }
    let total = 0;
    for (const n of nals) total += 4 + n.length;
    const out = new Uint8Array(total);
    let o = 0;
    for (const n of nals) {
      out[o++] = (n.length >>> 24) & 0xff;
      out[o++] = (n.length >>> 16) & 0xff;
      out[o++] = (n.length >>> 8) & 0xff;
      out[o++] = n.length & 0xff;
      out.set(n, o);
      o += n.length;
    }
    return out;
  }

  /** 找 starts[k] 的结束位置（下一个起始码处，含 00 00 00 01 的前导 0） */
  private prevStart(starts: number[], k: number, payload: Uint8Array): number {
    const next = starts[k];
    // 回退 00 00 00 01 的额外 0
    let end = next;
    let z = 0;
    let p = next - 3; // 至少留 00 00 01
    while (p - 1 >= 0 && payload[p - 1] === 0) { z++; p--; }
    return next - 3 - z;
  }

  private ensureDecoder(configPayload: Uint8Array) {
    if (this.decoder && this.decoder.state === "configured") return;
    if (typeof VideoDecoder === "undefined") {
      toast("当前 WebView 不支持 WebCodecs", "请更新系统 WebView2", true);
      return;
    }
    const parsed = this.avcCDescription(configPayload);
    if (!parsed) {
      this.hudUpdate("config 包中未找到 SPS/PPS");
      return;
    }
    this.decoder = new VideoDecoder({
      output: (frame) => this.onFrame(frame),
      error: (e) => {
        this.hudUpdate(`err=${e}`);
        toast("解码器错误", String(e), true);
      },
    });
    try {
      this.decoder.configure({
        codec: parsed.codec,
        description: parsed.avcC as BufferSource,
        optimizeForLatency: true, // 关键：<80ms 延迟
      });
      this.hudUpdate(`configure ${parsed.codec} desc=${parsed.avcC.length}B`);
    } catch (e) {
      this.hudUpdate(`configure 异常: ${e}`);
      toast("解码器配置失败", String(e), true);
    }
  }

  /** 从 Annex-B SPS 解析 profile/constraint/level，构造精确的 codec 串 */
  private codecFromSps(payload: Uint8Array): string | undefined {
    // 找 00 00 01 或 00 00 00 01 起始码后的 NAL
    let i = 0;
    while (i < payload.length - 4) {
      if (payload[i] === 0 && payload[i + 1] === 0 && payload[i + 2] === 1) {
        // NAL 布局：[起始码 00 00 01][NAL头][profile][compat][level]...
        const nal = payload[i + 3];
        if ((nal & 0x1f) === 7 && i + 7 <= payload.length) {
          const p = payload[i + 4], c = payload[i + 5], l = payload[i + 6];
          const hex = (b: number) => b.toString(16).padStart(2, "0").toUpperCase();
          return `avc1.${hex(p)}${hex(c)}${hex(l)}`;
        }
        i += 3;
      } else {
        i++;
      }
    }
    return undefined;
  }

  private firstChunkSeen = false;
  private decodedCount = 0;
  private cntConfig = 0;
  private cntKey = 0;
  private cntDelta = 0;
  /** configure() 异步生效期间到达的块，configured 后按序补喂 */
  private pendingChunks: { kind: number; pts: number; payload: Uint8Array }[] = [];
  private flushing = false;

  private hudLog: string[] = [];
  private hudUpdate(extra = "") {
    this.hud.classList.remove("hidden");
    this.hudLog.push(extra);
    if (this.hudLog.length > 6) this.hudLog.shift();
    this.hud.textContent =
      `cfg=${this.cntConfig} key=${this.cntKey} delta=${this.cntDelta} dec=${this.decodedCount} state=${this.decoder?.state ?? "null"} | ` +
      this.hudLog.join(" | ");
  }

  private onChunk(buf: Uint8Array) {
    if (buf.length < 9) return;
    const kind = buf[0];
    if (kind === 0) this.cntConfig++;
    else if (kind === 1) this.cntKey++;
    else this.cntDelta++;
    this.hudUpdate();
    const pts = Number(new DataView(buf.buffer, buf.byteOffset, buf.byteLength).getBigInt64(1, false));
    const payload = buf.subarray(9);
    try {
      if (kind === 0) {
        this.ensureDecoder(payload);
        return;
      }
      if (!this.decoder || this.decoder.state !== "configured") {
        // configure() 异步生效：先缓存媒体块（等首个 key 帧后由门控保证有序），就绪后补喂
        if (kind !== 0) {
          if (this.pendingChunks.length > 120) this.pendingChunks.shift();
          this.pendingChunks.push({ kind, pts, payload });
          this.tryFlushPending();
        }
        return;
      }
      this.tryFlushPending();
      if (!this.decoder || this.decoder.state !== "configured") {
        this.hudUpdate(`skip kind=${kind} (not configured)`);
        return;
      }
      let chunk: EncodedVideoChunk;
      try {
        chunk = new EncodedVideoChunk({
          type: kind === 1 ? "key" : "delta",
          timestamp: pts,
          data: this.annexbToAvcc(payload) as BufferSource,
        });
      } catch (e) {
        this.hudUpdate(`chunkErr kind=${kind}: ${e}`);
        return;
      }
      try {
        this.decoder.decode(chunk);
        this.decodedCount++;
        this.hudUpdate(`decoded kind=${kind} sz=${payload.length} q=${this.decoder.decodeQueueSize}`);
      } catch (e) {
        this.hudUpdate(`decodeErr kind=${kind}: ${e}`);
      }
      // 延迟估计：解码时间戳与本地时钟差（近似）
      if (pts > 0) {
        const nowUs = performance.now() * 1000;
        const d = Math.abs(nowUs - pts) / 1e6;
        if (d < 5000) this.latencyEMA = this.latencyEMA * 0.9 + d * 0.1;
      }
    } catch (e) {
      console.error("chunk", e);
    }
  }

  /** decoder configured 后按序补喂缓存的媒体块 */
  private tryFlushPending() {
    if (this.flushing || !this.decoder || this.decoder.state !== "configured") return;
    this.flushing = true;
    try {
      while (this.pendingChunks.length && this.decoder.state === "configured") {
        const c = this.pendingChunks.shift()!;
        const chunk = new EncodedVideoChunk({
          type: c.kind === 1 ? "key" : "delta",
          timestamp: c.pts,
          data: this.annexbToAvcc(c.payload) as BufferSource,
        });
        this.decoder.decode(chunk);
        this.decodedCount++;
        this.hudUpdate(`flushed kind=${c.kind} sz=${c.payload.length} q=${this.decoder.decodeQueueSize}`);
      }
    } catch (e) {
      this.hudUpdate(`flushErr=${e}`);
    }
    this.flushing = false;
  }

  private onFrame(frame: VideoFrame) {
    this.fitCanvas(frame.displayWidth, frame.displayHeight);
    const ctx = this.canvas.getContext("2d")!;
    ctx.drawImage(frame, 0, 0, this.canvas.width, this.canvas.height);
    frame.close();
    this.frameCount++;
    const now = performance.now();
    if (now - this.fpsTime > 1000) {
      const fps = (this.frameCount * 1000) / (now - this.fpsTime);
      this.hudUpdate(`${fps.toFixed(0)}fps · ${this.info.width}×${this.info.height} · lat≈${this.latencyEMA.toFixed(0)}ms`);
      this.frameCount = 0;
      this.fpsTime = now;
    }
  }

  private fitCanvas(w = this.info.width, h = this.info.height) {
    if (this.canvas.width !== w || this.canvas.height !== h) {
      this.canvas.width = w;
      this.canvas.height = h;
    }
  }

  // ---- 画布坐标 -> 设备坐标 ----
  private toDevice(clientX: number, clientY: number): [number, number] | null {
    const rect = this.canvas.getBoundingClientRect();
    const scale = Math.min(rect.width / this.info.width, rect.height / this.info.height);
    const dw = this.info.width * scale, dh = this.info.height * scale;
    const ox = rect.left + (rect.width - dw) / 2;
    const oy = rect.top + (rect.height - dh) / 2;
    const x = (clientX - ox) / scale;
    const y = (clientY - oy) / scale;
    if (x < 0 || y < 0 || x > this.info.width || y > this.info.height) return null;
    return [Math.round(x), Math.round(y)];
  }

  // ---- 反控输入 ----
  private bindInput() {
    const c = this.canvas;
    c.addEventListener("contextmenu", (e) => e.preventDefault());

    c.addEventListener("pointerdown", (e) => {
      if (!this.serial) return;
      c.focus();
      const p = this.toDevice(e.clientX, e.clientY);
      if (!p) return;
      if (e.button === 2) { void api.sendKey(this.serial, "back"); return; }
      if (e.button === 1) { void api.sendKey(this.serial, "home"); return; }
      this.pointerDown = true;
      c.setPointerCapture(e.pointerId);
      void api.sendTouch(this.serial, 0, p[0], p[1], this.info.width, this.info.height, 1, 1);
    });

    c.addEventListener("pointermove", (e) => {
      if (!this.pointerDown || !this.serial) return;
      const p = this.toDevice(e.clientX, e.clientY);
      if (!p) return;
      this.pendingMove = { x: p[0], y: p[1] };
      if (!this.moveScheduled) {
        this.moveScheduled = true;
        requestAnimationFrame(() => {
          this.moveScheduled = false;
          if (this.pendingMove && this.serial) {
            void api.sendTouch(this.serial, 2, this.pendingMove.x, this.pendingMove.y,
              this.info.width, this.info.height, 1, 1);
          }
        });
      }
    });

    const up = (e: PointerEvent) => {
      if (!this.pointerDown || !this.serial) return;
      this.pointerDown = false;
      const p = this.toDevice(e.clientX, e.clientY) ?? [0, 0];
      void api.sendTouch(this.serial, 1, p[0], p[1], this.info.width, this.info.height, 1, 0);
    };
    c.addEventListener("pointerup", up);
    c.addEventListener("pointercancel", up);

    c.addEventListener("wheel", (e) => {
      if (!this.serial) return;
      e.preventDefault();
      const p = this.toDevice(e.clientX, e.clientY) ?? [0, 0];
      void api.sendScroll(this.serial, p[0], p[1], this.info.width, this.info.height, e.deltaY);
    }, { passive: false });

    c.addEventListener("keydown", (e) => {
      if (!this.serial) return;
      e.preventDefault();
      if (e.ctrlKey && (e.key === "c" || e.key === "C")) {
        void api.deviceClipboard(this.serial, "get");
        return;
      }
      if (e.ctrlKey && (e.key === "v" || e.key === "V")) {
        navigator.clipboard.readText().then((t) => {
          if (t && this.serial) void api.deviceClipboard(this.serial, "set", t);
        }).catch(() => toast("无法读取本机剪贴板", "", true));
        return;
      }
      if (e.ctrlKey || e.metaKey || e.altKey) return;
      if (KEYCODE_MAP[e.key]) {
        void api.sendKey(this.serial, KEYCODE_MAP[e.key]);
        return;
      }
      if (e.key.length === 1) {
        void api.sendText(this.serial, e.key);
      }
    });
  }

  // ---- 拖拽传文件（Tauri webview 级 drag-drop，携带真实路径） ----
  private bindDragDrop() {
    void getCurrentWebview().onDragDropEvent((ev) => {
      const p = ev.payload;
      if (p.type === "over") {
        const rect = this.stage.getBoundingClientRect();
        const inside = p.position.x >= rect.left && p.position.x <= rect.right
          && p.position.y >= rect.top && p.position.y <= rect.bottom;
        this.overlay.classList.toggle("show", inside && !!this.serial);
      } else if (p.type === "drop") {
        this.overlay.classList.remove("show");
        if (this.serial && p.paths.length) {
          this.pushDropped(p.paths);
        }
      } else {
        this.overlay.classList.remove("show");
      }
    }).then((fn) => { this.unlistenDrag = fn; });
  }

  private async pushDropped(paths: string[]) {
    if (!this.serial) return;
    try {
      await api.pushFiles(this.serial, paths, store.settings.push_dir, "rename");
      toast(`正在发送 ${paths.length} 个文件`, `目标：${store.settings.push_dir}`);
    } catch (e) {
      toast("发送失败", String(e), true);
    }
  }

  // ---- 工具栏 ----
  toolbar(title: string): HTMLElement {
    const mk = (icon: string, label: string, onClick: () => void, primary = false) =>
      el("button", { class: primary ? "primary" : "", onclick: onClick, style: "display:inline-flex;align-items:center;gap:5px" },
        iconEl(icon, 13), document.createTextNode(label));
    return el("div", { class: "toolbar" },
      el("span", { class: "title" }, title),
      mk("back", "返回", () => this.serial && api.sendKey(this.serial, "back")),
      mk("home", "Home", () => this.serial && api.sendKey(this.serial, "home")),
      mk("recents", "后台", () => this.serial && api.sendKey(this.serial, "recents")),
      el("span", { class: "sep" }),
      mk("camera", "截图", async () => {
        if (!this.canvas.width) return;
        const b64 = this.canvas.toDataURL("image/png").split(",")[1];
        try {
          const r = await api.writeScreenshot(this.serial ?? "", b64);
          toast("截图已保存", String((r as { path?: string }).path ?? ""));
        } catch (e) { toast("截图失败", String(e), true); }
      }),
      mk("maximize", "全屏", () => {
        if (document.fullscreenElement) void document.exitFullscreen();
        else void this.stage.requestFullscreen();
      }),
      el("span", { class: "spacer" }),
      el("span", { class: "chip", style: "margin-right:6px" }, "拖文件进窗口即可发送"),
      mk("unplug", "断开投屏", () => this.close(), true),
    );
  }
}
