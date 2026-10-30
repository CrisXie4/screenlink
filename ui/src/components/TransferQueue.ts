// 底部传输队列：可折叠，默认收起；连接后显示入口
import { listen } from "@tauri-apps/api/event";
import { api, type JobSnapshot } from "../tauri";
import { el, fmtBytes, fmtSpeed, toast } from "../store";
import { iconEl } from "../icons";

export class TransferQueue {
  el: HTMLElement;
  private list = el("div", { class: "list hidden" });
  private countEl = el("span", {});
  private chevron = el("span", { class: "icon-only" }, iconEl("chevronUp", 13));
  private expanded = false;
  private jobs: JobSnapshot[] = [];

  constructor() {
    const bar = el("div", { class: "bar", onclick: () => this.toggle() },
      el("span", { class: "t" }, "传输队列"),
      this.countEl,
      el("span", { class: "spacer" }),
      this.chevron,
    );
    this.el = el("div", { class: "transfer" }, bar, this.list);
    this.render();
    void listen<JobSnapshot[]>("transfers", (ev) => {
      this.jobs = ev.payload;
      this.render();
    });
  }

  private toggle() {
    this.expanded = !this.expanded;
    this.list.classList.toggle("hidden", !this.expanded);
    this.chevron.innerHTML = iconEl(this.expanded ? "chevronDown" : "chevronUp", 13).innerHTML;
  }

  private render() {
    const active = this.jobs.filter((j) => j.state === "running" || j.state === "queued").length;
    this.countEl.textContent = this.jobs.length ? `${this.jobs.length} 个任务（${active} 个进行中）` : "暂无任务";
    this.list.innerHTML = "";
    if (!this.jobs.length) {
      this.list.append(el("div", { style: "padding:8px 0;color:var(--text-3)" },
        "拖文件到投屏窗口即可发送；也可在设备卡上使用发送/接收"));
      return;
    }
    for (const j of [...this.jobs].reverse()) this.list.append(this.jobRow(j));
  }

  private jobRow(j: JobSnapshot): HTMLElement {
    const stateText: Record<string, string> = {
      queued: "排队中", running: "传输中", done: "已完成", failed: "失败", cancelled: "已取消",
    };
    const speed = j.state === "running" && j.speed_bps > 0 ? ` ${fmtSpeed(j.speed_bps)}` : "";
    const eta = j.state === "running" && j.eta_secs > 1 ? ` 剩 ${Math.ceil(j.eta_secs)}s` : "";
    const size = j.total_bytes > 0 ? ` · ${fmtBytes(j.copied_bytes)}/${fmtBytes(j.total_bytes)}` : "";
    const channel = j.channel === "agent" ? "高速通道" : "adb";
    return el("div", { class: `job ${j.state}` },
      el("div", { class: "row" },
        el("span", { class: "title" }, j.title),
        el("span", { class: "stat" }, `${stateText[j.state]} · ${channel}${speed}${eta}${size}`),
        j.state === "running" || j.state === "queued"
          ? el("button", {
            class: "ghost small",
            onclick: () => void api.cancelTransfer(j.id).catch((e) => toast("取消失败", String(e), true)),
          }, "取消")
          : null,
        j.state === "failed"
          ? el("button", {
            class: "ghost small",
            onclick: () => {
              void api.retryTransfer(j.id).catch((e) => toast("重试失败", String(e), true));
            },
          }, "重试")
          : null,
      ),
      el("div", { class: "bar2" }, el("div", { style: `width:${j.percent}%` })),
      j.error ? el("div", { class: "errmsg" }, j.error) : null,
    );
  }
}
