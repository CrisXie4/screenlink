// 投屏会话逻辑层：与 DOM 解耦。
// 职责：WS 流接入、WebCodecs 解码、统计、画布坐标->设备坐标、反控输入、拖拽发送。
// 界面（空状态/loading/overlay）由 components/ScreenCanvas 负责，本层只操作 canvas。
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { api } from "./tauri";
import { store, toast } from "./store";

const KEYCODE_MAP: Record<string, string> = {
  Enter: "enter",
  Backspace: "backspace",
  Escape: "back",
  Tab: "tab",
  ArrowUp: "up",
  ArrowDown: "down",
  ArrowLeft: "left",
  ArrowRight: "right",
  Home: "home_key",
  End: "move_end",
  PageUp: "page_up",
  PageDown: "page_down",
};

export interface SessionCallbacks {
  /** 连接阶段推进（"正在启动服务…" / "正在获取画面…"） */
  onStep(step: string): void;
  /** 首帧解码完成，画面可用 */
  onStreaming(): void;
  /** 会话结束（reason 为空表示主动断开） */
  onClosed(reason?: string): void;
  /** 每秒统计 */
  onStats(stats: { fps: number; latencyMs: number; width: number; height: number }): void;
  /** 用户向画布拖入文件 */
  onDrop(paths: string[]): void;
}

export class MirrorSession {
  private canvas: HTMLCanvasElement;
  private cb: SessionCallbacks;
  private serial: string | null = null;
  private info = { width: 1080, height: 2400 };
  private decoder: VideoDecoder | null = null;
  private ws: WebSocket | null = null;
  private intentionalClose = false;
  private frameCount = 0;
  private fpsTime = 0;
  private latencyEMA = 0;
  private pointerDown = false;
  private pendingMove: { x: number; y: number } | null = null;
  private moveScheduled = false;
  private unlistenDrag: (() => void) | null = null;
  private detach: (() => void) | null = null;

  constructor(canvas: HTMLCanvasElement, cb: SessionCallbacks) {
    this.canvas = canvas;
    this.cb = cb;
    this.bindInput();
    this.bindDragDrop();
  }

  get active(): boolean {
    return this.serial !== null;
  }

  async connect(serial: string) {
    this.disconnect();
    this.intentionalClose = false;
    this.serial = serial;
    this.cb.onStep("正在启动服务…");
    try {
      const res = await api.startMirror(serial);
      store.wsPort = res.wsPort;
    } catch (e) {
      this.serial = null;
      this.cb.onClosed(String(e));
      return;
    }
    if (!this.serial) return; // 期间被断开
    this.cb.onStep("正在获取画面…");
    this.connectWs();
  }

  disconnect() {
    const s = this.serial;
    this.serial = null;
    if (s) void api.stopMirror(s).catch(() => {});
    this.intentionalClose = true;
    this.ws?.close();
    this.ws = null;
    if (this.decoder) {
      try {
        this.decoder.close();
      } catch {
        /* already closed */
      }
      this.decoder = null;
    }
    this.detach?.();
    this.detach = null;
    this.unlistenDrag?.();
    this.unlistenDrag = null;
  }

  // ---- WS 流接入（Rust 桥：avcC description 走 info，媒体帧为 avcc） ----
  private connectWs() {
    if (!this.serial) return;
    const ws = new WebSocket(`ws://127.0.0.1:${store.wsPort}`);
    ws.binaryType = "arraybuffer";
    this.ws = ws;
    ws.onopen = () => ws.send(`mirror:${this.serial}`);
    ws.onmessage = (ev) => {
      if (typeof ev.data === "string") {
        try {
          const j = JSON.parse(ev.data);
          if (j.info) {
            this.info = j.info;
            this.fitCanvas();
            if (j.info.descriptionB64 && j.info.codec) {
              this.configureFromDescription(j.info.codec, j.info.descriptionB64);
            }
          }
        } catch {
          /* ignore */
        }
        return;
      }
      this.onChunk(new Uint8Array(ev.data));
    };
    ws.onclose = () => {
      if (this.serial && !this.intentionalClose) {
        this.cb.onClosed("设备连接中断");
      }
      this.serial = null;
    };
  }

  private configureFromDescription(codec: string, descriptionB64: string) {
    if (this.decoder && this.decoder.state === "configured") return;
    const bin = atob(descriptionB64);
    const desc = new Uint8Array(bin.length);
    for (let i = 0; i < bin.length; i++) desc[i] = bin.charCodeAt(i);
    this.decoder = new VideoDecoder({
      output: (frame) => this.onFrame(frame),
      error: () => this.cb.onClosed("解码器错误"),
    });
    this.decoder.configure({
      codec,
      description: desc as BufferSource,
      optimizeForLatency: true, // 关键：<80ms 延迟
    });
  }

  private onChunk(buf: Uint8Array) {
    if (buf.length < 9) return;
    const kind = buf[0];
    const pts = Number(
      new DataView(buf.buffer, buf.byteOffset, buf.byteLength).getBigInt64(1, false),
    );
    const payload = buf.subarray(9);
    if (!this.decoder || this.decoder.state !== "configured") return;
    try {
      const chunk = new EncodedVideoChunk({
        type: kind === 1 ? "key" : "delta",
        timestamp: pts,
        data: payload as BufferSource,
      });
      this.decoder.decode(chunk);
    } catch {
      /* 单块失败不中断会话 */
    }
  }

  private onFrame(frame: VideoFrame) {
    const first = this.frameCount === 0;
    this.fitCanvas(frame.displayWidth, frame.displayHeight);
    const ctx = this.canvas.getContext("2d")!;
    ctx.drawImage(frame, 0, 0, this.canvas.width, this.canvas.height);
    frame.close();
    if (first) this.cb.onStreaming();
    this.frameCount++;
    const now = performance.now();
    if (now - this.fpsTime > 1000) {
      const fps = (this.frameCount * 1000) / (now - this.fpsTime);
      this.cb.onStats({
        fps,
        latencyMs: this.latencyEMA,
        width: this.info.width,
        height: this.info.height,
      });
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
    const dw = this.info.width * scale;
    const dh = this.info.height * scale;
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
      if (e.button === 2) {
        void api.sendKey(this.serial, "back");
        return;
      }
      if (e.button === 1) {
        void api.sendKey(this.serial, "home");
        return;
      }
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
            void api.sendTouch(
              this.serial,
              2,
              this.pendingMove.x,
              this.pendingMove.y,
              this.info.width,
              this.info.height,
              1,
              1,
            );
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
        navigator.clipboard
          .readText()
          .then((t) => {
            if (t && this.serial) void api.deviceClipboard(this.serial, "set", t);
          })
          .catch(() => toast("无法读取本机剪贴板", "", true));
        return;
      }
      if (e.ctrlKey || e.metaKey || e.altKey) return;
      if (KEYCODE_MAP[e.key]) {
        void api.sendKey(this.serial, KEYCODE_MAP[e.key]);
        return;
      }
      if (e.key.length === 1) void api.sendText(this.serial, e.key);
    });
  }

  // ---- 拖拽传文件（Tauri webview 级 drag-drop，携带真实路径） ----
  private bindDragDrop() {
    void getCurrentWebview()
      .onDragDropEvent((ev) => {
        const p = ev.payload;
        if (p.type === "drop") {
          const rect = this.canvas.getBoundingClientRect();
          const inside =
            p.position.x >= rect.left &&
            p.position.x <= rect.right &&
            p.position.y >= rect.top &&
            p.position.y <= rect.bottom;
          if (this.serial && inside && p.paths.length) this.cb.onDrop(p.paths);
        }
      })
      .then((fn) => {
        this.unlistenDrag = fn;
      });
  }

  async pushDropped(paths: string[]) {
    if (!this.serial) return;
    try {
      await api.pushFiles(this.serial, paths, store.settings.push_dir, "rename");
      toast(`正在发送 ${paths.length} 个文件`, `目标：${store.settings.push_dir}`);
    } catch (e) {
      toast("发送失败", String(e), true);
    }
  }

  // ---- 工具动作（供工具条调用） ----
  sendKey(key: string) {
    if (this.serial) void api.sendKey(this.serial, key);
  }

  async screenshot() {
    if (!this.canvas.width) return;
    const b64 = this.canvas.toDataURL("image/png").split(",")[1];
    try {
      const r = await api.writeScreenshot(this.serial ?? "", b64);
      toast("截图已保存", String((r as { path?: string }).path ?? ""));
    } catch (e) {
      toast("截图失败", String(e), true);
    }
  }

  toggleFullscreen(area: HTMLElement) {
    if (document.fullscreenElement) void document.exitFullscreen();
    else void area.requestFullscreen();
  }

  /** MCP 快速截图：把当前画布帧上报给核心（供 AI screenlink_screenshot 复用） */
  async uploadFrame(serial: string) {
    if (!this.canvas.width || !this.serial) return;
    const b64 = this.canvas.toDataURL("image/jpeg", 0.8).split(",")[1];
    await api.uploadFrame(serial, "image/jpeg", b64).catch(() => {});
  }
}
