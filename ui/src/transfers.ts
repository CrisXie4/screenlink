// 传输队列抽屉：进度、速度、剩余时间、取消
import { api, type JobSnapshot } from "./tauri";
import { el, fmtBytes, fmtSpeed } from "./store";
import { iconEl } from "./icons";

export function createTransferDrawer(onOpenMirror: () => void) {
  const list = el("div", { class: "transfer-list" });
  const el_ = el("div", { class: "transfer-drawer hidden" },
    el("div", { class: "head" },
      el("span", {}, "传输队列"),
      el("button", { class: "ghost small", onclick: () => el_.classList.add("hidden") }, iconEl("x", 12)),
    ),
    list,
  );

  function jobRow(j: JobSnapshot): HTMLElement {
    const stateText: Record<string, string> = {
      queued: "排队中", running: "传输中", done: "已完成", failed: "失败", cancelled: "已取消",
    };
    const speed = j.state === "running" ? ` ${fmtSpeed(j.speed_bps)}` : "";
    const eta = j.state === "running" && j.eta_secs > 1 ? ` 剩 ${Math.ceil(j.eta_secs)}s` : "";
    const size = j.total_bytes > 0 ? ` · ${fmtBytes(j.copied_bytes)}/${fmtBytes(j.total_bytes)}` : "";
    const row = el("div", { class: `job ${j.state}` },
      el("div", { class: "row" },
        el("span", { class: "title" }, j.title),
        el("span", { class: "stat" }, stateText[j.state] + speed + eta + size),
        j.state === "running" || j.state === "queued"
          ? el("button", { class: "ghost small", onclick: () => void api.cancelTransfer(j.id) }, "取消")
          : null,
      ),
      el("div", { class: "bar" }, el("div", { style: `width:${j.percent}%` })),
      j.error ? el("div", { style: "color:var(--danger);font-size:11px;margin-top:4px" }, j.error) : null,
    );
    return row;
  }

  return {
    el: el_,
    show() {
      el_.classList.remove("hidden");
      onOpenMirror();
    },
    update(jobs: JobSnapshot[]) {
      list.innerHTML = "";
      if (!jobs.length) {
        list.append(el("div", { style: "padding:10px;color:var(--text-faint);font-size:11.5px" }, "暂无传输任务"));
        return;
      }
      // 最近的在上
      for (const j of [...jobs].reverse()) list.append(jobRow(j));
    },
  };
}
