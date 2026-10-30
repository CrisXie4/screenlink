// 设置 / AI 访问模态框
import { api, type Settings } from "../tauri";
import { el, store, toast } from "../store";
import { iconEl } from "../icons";

export function openSettingsDialog(onChanged: () => void) {
  const s = store.settings;

  const sizeSel = el("select", {},
    ...[0, 800, 1080, 1280, 1600, 1920].map((v) =>
      el("option", { value: String(v), ...(v === s.max_size ? { selected: "true" } : {}) },
        v === 0 ? "原生分辨率" : `≤ ${v}px`))) as HTMLSelectElement;
  const bitrate = el("input", { type: "range", min: "2000000", max: "50000000", step: "1000000", value: String(s.bit_rate) }) as HTMLInputElement;
  const bitrateLabel = el("span", { style: "font-size:12px;color:var(--text-2);width:64px" }, `${(s.bit_rate / 1e6).toFixed(0)} Mbps`);
  bitrate.addEventListener("input", () => { bitrateLabel.textContent = `${(Number(bitrate.value) / 1e6).toFixed(0)} Mbps`; });
  const fpsSel = el("select", {},
    ...[15, 24, 30, 60, 120].map((v) =>
      el("option", { value: String(v), ...(v === s.max_fps ? { selected: "true" } : {}) }, `${v} fps`))) as HTMLSelectElement;
  const kfSel = el("select", {},
    ...[1, 2, 4, 10].map((v) =>
      el("option", { value: String(v), ...(v === s.keyframe_interval ? { selected: "true" } : {}) },
        v === 1 ? "1 秒（接入最快）" : `${v} 秒（更省电省带宽）`))) as HTMLSelectElement;

  // 画质预设：老电脑/低端机选「流畅」；点击填充下方参数，保存后下次投屏生效
  const PRESETS = [
    { name: "流畅", size: "1280", rate: "4000000", fps: "30", kf: "4", hint: "老电脑 / 低端手机" },
    { name: "均衡", size: "1600", rate: "8000000", fps: "60", kf: "2", hint: "推荐" },
    { name: "高清", size: "1920", rate: "16000000", fps: "60", kf: "1", hint: "高配设备" },
  ];
  const presetRow = el("div", { class: "field" },
    el("div", {}, el("div", {}, "画质预设"),
      el("div", { class: "hint" }, "点击填充参数，保存后下次投屏生效")),
    el("div", { style: "display:flex;gap:6px" },
      ...PRESETS.map((p) => el("button", {
        class: "small",
        title: p.hint,
        onclick: () => {
          sizeSel.value = p.size;
          bitrate.value = p.rate;
          bitrateLabel.textContent = `${(Number(p.rate) / 1e6).toFixed(0)} Mbps`;
          fpsSel.value = p.fps;
          kfSel.value = p.kf;
        },
      }, p.name))));

  const pushDir = el("input", { type: "text", value: s.push_dir }) as HTMLInputElement;
  const pullDir = el("input", { type: "text", value: s.pull_dir, placeholder: "默认下载目录" }) as HTMLInputElement;
  const clipSwitch = switchInput(s.clipboard_sync);
  const audioSwitch = switchInput(s.audio);
  const autoSwitch = switchInput(s.auto_switch_wifi);

  const saveBtn = el("button", {
    class: "primary",
    onclick: async () => {
      const next: Settings = {
        ...s,
        max_size: Number(sizeSel.value),
        bit_rate: Number(bitrate.value),
        max_fps: Number(fpsSel.value),
        keyframe_interval: Number(kfSel.value),
        push_dir: pushDir.value.trim() || "/sdcard/Download/ScreenLink",
        pull_dir: pullDir.value.trim(),
        clipboard_sync: clipSwitch.checked,
        audio: audioSwitch.checked,
        auto_switch_wifi: autoSwitch.checked,
      };
      try {
        await api.setSettings(next);
        await api.setClipboardSync(clipSwitch.checked);
        store.settings = next;
        toast("设置已保存", "分辨率/码率下次投屏生效");
      } catch (e) {
        toast("保存失败", String(e), true);
      }
    },
  }, "保存");

  const aiCard = el("div", { class: "card" });
  renderAi(aiCard);
  const auditCard = el("div", { class: "card" });
  renderAudit(auditCard);

  const mask = el("div", { class: "modal-mask" }, el("div", { class: "modal", style: "width:640px" },
    el("div", { style: "display:flex;justify-content:space-between;align-items:center" },
      el("h3", {}, "设置 / AI"),
      el("button", { class: "ghost icon-only", onclick: () => mask.remove() }, iconEl("x", 14)),
    ),
    el("div", { class: "card" },
      el("h3", {}, "投屏与传输"),
      presetRow,
      field("分辨率上限", sizeSel),
      field("视频码率", el("span", { style: "display:flex;align-items:center;gap:8px" }, bitrate, bitrateLabel)),
      field("帧率上限", fpsSel, "低配设备建议 30 fps"),
      field("关键帧间隔", kfSel, "间隔越大越省电省带宽，重新接入恢复画面越慢"),
      field("推送目录（手机）", pushDir),
      field("接收目录（电脑）", pullDir),
      field("剪贴板双向同步", clipSwitch, "电脑与手机复制粘贴自动同步"),
      field("音频转发到电脑", audioSwitch, "开启后手机媒体声音会在电脑播放（手机同时出声）；关闭则仅手机出声。需 Android 11+，下次投屏生效"),
      field("拔线自动切 WiFi", autoSwitch, "USB 断开后自动尝试无线重连"),
      el("div", { style: "text-align:right;margin-top:8px" }, saveBtn),
    ),
    aiCard,
    auditCard,
  ));
  mask.addEventListener("click", (e) => e.target === mask && mask.remove());
  document.body.append(mask);
  void onChanged;
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

function renderAi(card: HTMLElement) {
  card.innerHTML = "";
  void api.getAiInfo().then((info) => {
    const toggle = switchInput(info.enabled);
    toggle.addEventListener("change", async () => {
      if (toggle.checked) {
        await api.enableAi();
        toast("AI 访问已开启", "配置片段已生成");
      } else {
        await api.disableAi();
        toast("AI 访问已撤销", "已连接的 AI 客户端下一个请求将被拒绝");
      }
      store.aiEnabled = toggle.checked;
      renderAi(card);
    });
    card.append(
      el("h3", {}, "AI 访问（MCP）"),
      field("允许 AI 操控手机", toggle, "AI 可截屏、点击、滑动、传文件；全部操作被审计记录"),
    );
    if (info.enabled) {
      card.append(
        el("div", { style: "font-size:12px;color:var(--text-2);margin:4px 0" },
          "复制到 Claude Desktop / Cursor 的 MCP 配置："),
        el("pre", { class: "snippet" }, info.snippet),
        el("div", { style: "text-align:right;margin-top:8px" },
          el("button", {
            class: "small primary",
            onclick: async () => {
              await navigator.clipboard.writeText(info.snippet);
              toast("已复制 MCP 配置");
            },
          }, "复制配置"),
        ),
      );
    }
  });
}

function renderAudit(card: HTMLElement) {
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
      if (!r.entries.length) {
        tbody.append(el("tr", {}, el("td", { colspan: "6", style: "color:var(--text-3)" }, "暂无 AI 操作记录")));
      }
      for (const e of r.entries) {
        tbody.append(el("tr", {},
          el("td", {}, e.ts),
          el("td", { class: "tool" }, e.tool),
          el("td", {}, e.device),
          el("td", { style: e.ok ? "color:var(--ok)" : "color:var(--danger)" }, e.ok ? "成功" : `失败 ${e.detail ?? ""}`),
          el("td", {}, `${e.took_ms}ms`),
          el("td", { style: "max-width:200px;overflow:hidden;text-overflow:ellipsis;white-space:nowrap" }, JSON.stringify(e.args)),
        ));
      }
    } catch { /* ignore */ }
  };
  card.append(
    el("div", { style: "display:flex;justify-content:space-between;align-items:center;margin-bottom:8px" },
      el("h3", { style: "margin:0" }, "AI 操作审计"),
      el("div", { style: "display:flex;gap:6px" },
        el("button", { class: "small", onclick: () => void refresh() }, "刷新"),
        el("button", {
          class: "small",
          onclick: async () => { await api.clearAudit(); await refresh(); },
        }, "清空"),
      ),
    ),
    table,
  );
  void refresh();
}
