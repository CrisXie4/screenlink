// 内联 SVG 图标集（lucide 风格描边图标，跟随 currentColor）
// 用法：iconEl("battery") 返回可直接 append 的元素

const PATHS: Record<string, string> = {
  // 电池
  battery:
    '<rect x="2" y="8" width="16" height="8" rx="2"/><path d="M22 11v2"/><path d="M6 11v2"/><path d="M10 11v2"/>',
  // 返回（左箭头）
  back: '<path d="M19 12H5"/><path d="m12 19-7-7 7-7"/>',
  // Home（圆形按键）
  home: '<circle cx="12" cy="12" r="9"/><circle cx="12" cy="12" r="3"/>',
  // 后台/多任务（方框）
  recents: '<rect x="4" y="4" width="16" height="16" rx="2"/>',
  // 相机
  camera:
    '<path d="M14.5 4h-5L7 7H4a2 2 0 0 0-2 2v9a2 2 0 0 0 2 2h16a2 2 0 0 0 2-2V9a2 2 0 0 0-2-2h-3l-2.5-3z"/><circle cx="12" cy="13" r="3"/>',
  // 全屏（四角展开）
  maximize:
    '<path d="M8 3H5a2 2 0 0 0-2 2v3"/><path d="M21 8V5a2 2 0 0 0-2-2h-3"/><path d="M3 16v3a2 2 0 0 0 2 2h3"/><path d="M16 21h3a2 2 0 0 0 2-2v-3"/>',
  // 设置齿轮
  settings:
    '<path d="M12.22 2h-.44a2 2 0 0 0-2 2v.18a2 2 0 0 1-1 1.73l-.43.25a2 2 0 0 1-2 0l-.15-.08a2 2 0 0 0-2.73.73l-.22.38a2 2 0 0 0 .73 2.73l.15.1a2 2 0 0 1 1 1.72v.51a2 2 0 0 1-1 1.74l-.15.09a2 2 0 0 0-.73 2.73l.22.38a2 2 0 0 0 2.73.73l.15-.08a2 2 0 0 1 2 0l.43.25a2 2 0 0 1 1 1.73V20a2 2 0 0 0 2 2h.44a2 2 0 0 0 2-2v-.18a2 2 0 0 1 1-1.73l.43-.25a2 2 0 0 1 2 0l.15.08a2 2 0 0 0 2.73-.73l.22-.39a2 2 0 0 0-.73-2.73l-.15-.08a2 2 0 0 1-1-1.74v-.5a2 2 0 0 1 1-1.74l.15-.09a2 2 0 0 0 .73-2.73l-.22-.38a2 2 0 0 0-2.73-.73l-.15.08a2 2 0 0 1-2 0l-.43-.25a2 2 0 0 1-1-1.73V4a2 2 0 0 0-2-2z"/><circle cx="12" cy="12" r="3"/>',
  // 关闭
  x: '<path d="M18 6 6 18"/><path d="m6 6 12 12"/>',
  // 勾
  check: '<path d="M20 6 9 17l-5-5"/>',
  // 叉
  xSmall: '<path d="m18 6-12 12"/><path d="m6 6 12 12"/>',
  // 状态点
  dot: '<circle cx="12" cy="12" r="5" fill="currentColor" stroke="none"/>',
  // 断开（插头斜线）—— 断开投屏按钮
  unplug: '<path d="m19 5 3-3"/><path d="m2 22 3-3"/><path d="M6.929 7.929 7.75 4.25a1.45 1.45 0 0 1 2.46-.66l.35.35a1.45 1.45 0 0 0 2.05 0l2.2-2.2a1.45 1.45 0 0 1 2.06 0l3.49 3.5a1.45 1.45 0 0 1 0 2.06l-2.2 2.2a1.45 1.45 0 0 0 0 2.05l.35.35a1.45 1.45 0 0 1-.66 2.46l-3.68.82"/><path d="M2.5 16.5a4.95 4.95 0 0 1 7-7l.7.7a4.95 4.95 0 0 1 0 7l-3.5 3.5a4.95 4.95 0 0 1-7 0 4.95 4.95 0 0 1 0-7Z" transform="translate(2 2) scale(0.75)"/>',
  // 发送（上传箭头）
  send: '<path d="M12 17V3"/><path d="m6 9 6-6 6 6"/><path d="M19 21H5"/>',
  // 接收（下载箭头）
  receive: '<path d="M12 3v14"/><path d="m6 15 6 6 6-6"/><path d="M19 3H5"/>',
  // 折叠箭头
  chevronUp: '<path d="m18 15-6-6-6 6"/>',
  chevronDown: '<path d="m6 9 6 6 6-6"/>',
  // 左侧面板
  panelLeft: '<rect x="3" y="3" width="18" height="18" rx="2"/><path d="M9 3v18"/>',
  // 播放（连接）
  play: '<polygon points="6 3 20 12 6 21 6 3" fill="currentColor" stroke="none"/>',
  // 数据线（USB）
  cable: '<path d="M12 22v-5"/><path d="M9 8V2"/><path d="M15 8V2"/><path d="M18 8v5a4 4 0 0 1-4 4h-4a4 4 0 0 1-4-4V8Z"/>',
  // WiFi
  wifi: '<path d="M5 13a10 10 0 0 1 14 0"/><path d="M8.5 16.5a5 5 0 0 1 7 0"/><path d="M2 8.82a15 15 0 0 1 20 0"/><circle cx="12" cy="20" r="1" fill="currentColor" stroke="none"/>',
  // 刷新
  refresh: '<path d="M3 12a9 9 0 0 1 15-6.7L21 8"/><path d="M21 3v5h-5"/><path d="M21 12a9 9 0 0 1-15 6.7L3 16"/><path d="M3 21v-5h5"/>',
  // 加载圈
  loader: '<path d="M21 12a9 9 0 1 1-6.2-8.56"/>',
  // 手机
  phone: '<rect x="7" y="2" width="10" height="20" rx="2"/><circle cx="12" cy="18" r="1"/>',
  // 镜像/投屏（显示器+手机）
  mirror:
    '<rect x="2" y="4" width="13" height="10" rx="2"/><path d="M9 18h4"/><path d="M6 18c0-2 2-4 4-4"/><rect x="16" y="9" width="6" height="11" rx="1.5"/>',
};

export function iconSvg(name: keyof typeof PATHS | string, size = 14): string {
  const p = PATHS[name] ?? "";
  return `<svg xmlns="http://www.w3.org/2000/svg" width="${size}" height="${size}" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" style="vertical-align:-2px">${p}</svg>`;
}

/** 返回带内联 SVG 的元素 */
export function iconEl(name: string, size = 14): HTMLElement {
  const span = document.createElement("span");
  span.innerHTML = iconSvg(name, size);
  return span;
}
