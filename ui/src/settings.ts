// 设置 + AI 访问面板
import { api, type AuditEntry, type Settings } from "./tauri";
import { el, store, toast } from "./store";
import { iconEl } from "./icons";

export function createSettingsView(): HTMLElement {
  const scroll = el("div", { class: "panel-scroll" });
  const inner = el("div", { class: "panel-narrow" },
    el("h2", {}, "设置"),
    el("p", { class: "desc" }, "投屏质量、传输目录与 AI 访问控制"));

  // ---- 投屏质量 ----
  const s = store.settings;
  const sizeSel = el("select", {},
    ...[0, 800, 1080, 1280, 1600, 1920].map((v) =>
      el("option", { value: String(v), ...(v === s.max_size ? { selected: "true" } : {}) },
        v === 0 ? "原生分辨率" : `≤ ${v}px`))) as HTMLSelectElement;
  const bitrate = el("input", { type: "range", min: "2000000", max: "50000000", step: "1000000", value: String(s.bit_rate) }) as HTMLInputElement;
  const bitrateLabel = el("span", { style: "font-family:var(--mono);font-size:11.5px;color:var(--text-dim);width:60px" }, `${(s.bit_rate / 1e6).toFixed(0)} Mbps`);
  bitrate.addEventListener("input", () => { bitrateLabel.textContent = `${(Number(bitrate.value) / 1e6).toFixed(0)} Mbps`; });

  const pushDir = el("input", { type: "text", value: s.push_dir }) as HTMLInputElement;
  const pullDir = el("input", { type: "text", value: s.pull_dir, placeholder: "默认下载目录" }) as HTMLInputElement;

  const clipSwitch = switchInput(s.clipboard_sync);
  const autoSwitch = switchInput(s.auto_switch_wifi);

  const saveBtn = el("button", {
    class: "primary",
    onclick: async () => {
      const next: Settings = {
        ...s,
        max_size: Number(sizeSel.value),
        bit_rate: Number(bitrate.value),
        push_dir: pushDir.value.trim() || "/sdcard/Download/ScreenLink",
        pull_dir: pullDir.value.trim(),
        clipboard_sync: clipSwitch.checked,
        auto_switch_wifi: autoSwitch.checked,
      };
      try {
        await api.setSettings(next);
        await api.setClipboardSync(clipSwitch.checked);
        store.settings = next;
        toast("设置已保存", "码率/分辨率更改将在下次投屏生效");
      } catch (e) { toast("保存失败", String(e), true); }
    },
  }, "保存设置");

  inner.append(el("div", { class: "card" },
    el("h3", {}, "投屏质量"),
    field("分辨率上限", sizeSel),
    field("视频码率", el("span", { style: "display:flex;align-items:center;gap:8px" }, bitrate, bitrateLabel)),
    field("推送目录（手机）", pushDir),
    field("接收目录（电脑）", pullDir),
    field("剪贴板双向同步", clipSwitch, "电脑 ↔ 手机复制粘贴自动同步"),
    field("拔线自动切 WiFi", autoSwitch, "USB 断开后自动尝试无线重连"),
    el("div", { style: "margin-top:10px;text-align:right" }, saveBtn),
  ));

  // ---- AI 访问 ----
  const aiCard = el("div", { class: "card" });
  renderAiCard(aiCard);
  inner.append(aiCard);

  // ---- 审计日志 ----
  const auditCard = el("div", { class: "card" });
  renderAudit(auditCard);
  inner.append(auditCard);

  scroll.append(inner);
  return scroll;
}

function field(label: string, control: HTMLElement, hint = ""): HTMLElement {
  return el("div", { class: "field" },
    el("div", {},
      el("div", { class: "label" }, label),
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

function renderAiCard(card: HTMLElement) {
  card.innerHTML = "";
  void api.getAiInfo().then((info) => {
    const toggle = switchInput(info.enabled);
    toggle.addEventListener("change", async () => {
      if (toggle.checked) {
        await api.enableAi();
        toast("AI 访问已开启", "配置片段已生成，可复制到 AI 客户端");
      } else {
        await api.disableAi();
        toast("AI 访问已撤销", "已连接的 AI 客户端下一个请求将被拒绝");
      }
      renderAiCard(card);
    });
    card.append(
      el("h3", {}, "AI 访问（MCP Server）"),
      field("允许 AI 操控手机", toggle, "AI 可截屏、点击、滑动、传文件；全部操作被审计记录"),
    );
    if (info.enabled) {
      const pre = el("pre", { class: "snippet", }, info.snippet);
      card.append(
        el("div", { style: "font-size:11.5px;color:var(--text-dim);margin:6px 0 4px" },
          "复制以下片段到 Claude Desktop / Cursor 的 MCP 配置："),
        pre,
        el("div", { style: "display:flex;gap:8px;margin-top:8px" },
          el("button", {
            class: "small primary",
            onclick: async () => {
              await navigator.clipboard.writeText(info.snippet);
              toast("已复制 MCP 配置");
            },
          }, "复制配置"),
        ),
      );
    } else {
      card.append(el("div", { style: "font-size:11.5px;color:var(--text-faint);margin-top:4px" },
        "开启后生成 screenlink-mcp 配置片段；随时可撤销访问。"));
    }
  });
}

function renderAudit(card: HTMLElement) {
  card.innerHTML = "";
  const tbody = el("tbody", {});
  const table = el("table", { class: "audit" },
    el("thead", {}, el("tr", {},
      el("th", {}, "时间"), el("th", {}, "工具"), el("th", {}, "设备"),
      el("th", {}, "结果"), el("th", {}, "耗时"), el("th", {}, "参数"))),
    tbody);
  const refresh = async () => {
    try {
      const r = await api.getAudit(100);
      tbody.innerHTML = "";
      const entries: AuditEntry[] = r.entries;
      if (!entries.length) {
        tbody.append(el("tr", {}, el("td", { colspan: "6", style: "color:var(--text-faint)" }, "暂无 AI 操作记录")));
      }
      for (const e of entries) {
        tbody.append(el("tr", {},
          el("td", {}, e.ts),
          el("td", { class: "tool" }, e.tool),
          el("td", {}, e.device),
          e.ok
            ? el("td", { class: "ok", style: "white-space:nowrap" }, iconEl("check", 12))
            : el("td", { class: "fail", style: "white-space:nowrap" }, iconEl("xSmall", 12), document.createTextNode(` ${e.detail ?? ""}`)),
          el("td", {}, `${e.took_ms}ms`),
          el("td", { style: "max-width:220px;overflow:hidden;text-overflow:ellipsis;white-space:nowrap" },
            JSON.stringify(e.args)),
        ));
      }
    } catch { /* ignore */ }
  };
  card.append(
    el("div", { style: "display:flex;justify-content:space-between;align-items:center;margin-bottom:10px" },
      el("h3", { style: "margin:0" }, "AI 操作审计"),
      el("div", { style: "display:flex;gap:6px" },
        el("button", { class: "small", onclick: () => void refresh() }, "刷新"),
        el("button", {
          class: "small danger",
          onclick: async () => { await api.clearAudit(); await refresh(); },
        }, "清空"),
      ),
    ),
    table,
  );
  void refresh();
}
