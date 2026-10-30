# ScreenLink

安卓 ↔ 电脑互联桌面端：低延迟投屏、电脑反控手机、拖拽传文件、剪贴板同步，并内置 MCP Server 把手机能力暴露给 AI 客户端（Claude Desktop / Cursor 等）。

设计目标：轻量（安装包小、空闲内存 <150MB）、精致（现代深色 UI）、快（投屏延迟 <80ms、AI 截图 <60ms、输入注入 <20ms）。

## 架构

```
crates/
  screenlink-core/    核心库（与 UI 解耦，可独立复用）
    adb.rs            ADB 封装：设备列表/连接/配对/tcpip/push-pull 进度/shell/截图/uiautomator
    device.rs         设备模型（USB / WiFi / 模拟器）与 adb devices -l、mDNS 输出解析
    scrcpy/           Scrcpy 协议客户端
      protocol.rs     v2.7 线协议：控制消息编码、设备消息解析、server 参数（含逐字节注释）
      session.rs      会话生命周期：jar 推送、adb forward 隧道、三条 socket、
                      视频泵（帧头解析/广播）、控制通道、设备事件、config 包缓存
    ws.rs             WebSocket 流桥：把 H.264 块推给 webview（WebCodecs 硬解），
                      SPS/PPS 补发 + 关键帧门控
    transfer.rs       传输队列：进度/速度/剩余时间/取消、冲突策略（覆盖/跳过/重命名）
    discovery.rs      局域网发现：adb mDNS（无线调试广播）+ UDP 广播 + 5555 子网扫描
    clipboard.rs      剪贴板双向同步（arboard 轮询 + 回环去重）
    ipc.rs            GUI <-> MCP 本地快速通道（127.0.0.1:27190，JSON 行协议）+ uiautomator 解析
    auth.rs           AI 访问授权：token 生成/校验/撤销 + MCP 配置片段生成
    audit.rs          AI 操作审计日志（JSONL）
  screenlink-app/     Tauri 2 桌面应用（状态编排 + 命令层 + 后台任务）
  screenlink-mcp/     MCP Server（stdio + HTTP 双传输，9 个 screenlink_* 工具 + screen resource）
  screenlink-cli      命令行调试工具（devices/mirror/screenshot/push/pull/page/wifi-scan...）
ui/                   前端（Vite + 原生 TypeScript，无框架）
  src/mirror.ts       WebCodecs 解码渲染 + 电脑反控（鼠标/键盘/滚轮）+ 拖拽传文件
assets/               scrcpy-server-v2.7.jar（Apache-2.0，来自 Genymobile/scrcpy 官方发布）
```

### 数据通路

```
手机 scrcpy-server (H.264)
   └─ adb forward TCP 隧道（video / control 两条 socket）
        ├─ 视频泵 ── WS 桥(127.0.0.1:27193) ── WebCodecs 硬解 ── Canvas 渲染
        └─ 控制通道 ←─ UI/IPC 注入（INJECT_TOUCH/TEXT/KEYCODE/CLIPBOARD）
AI (MCP stdio/HTTP) ──> screenlink-mcp ──> GUI IPC(:27190) 快速通道（复用投屏帧与控制 socket）
                                      └──> ADB 慢速兜路（GUI 未运行时）
```

## 构建

依赖：Rust 1.85+（MSVC）、Node 18+、Android platform-tools（adb）、WebView2 运行时（Win10/11 自带）。

> 手机端伴侣 App（Kotlin）在本仓库的 [`android-app`](https://github.com/CrisXie4/screenlink/tree/android-app) 分支（`android/` 工程），不在 main 上。

```powershell
# 1. 前端
cd ui && npm install && npm run build && cd ..

# 2. 全量构建（debug / release）
cargo build --workspace
cargo build --release --workspace

# 产物
#   target/debug/screenlink-app.exe      桌面应用
#   target/debug/screenlink-mcp.exe      MCP Server（给 AI 客户端用）
#   target/debug/screenlink-cli.exe      调试 CLI
```

打包安装包（NSIS）：

```powershell
cargo install tauri-cli --version "^2"
cargo tauri build
```

## 使用指南

1. 手机开启「USB 调试」（设置 → 关于手机 → 连点版本号 7 次 → 开发者选项 → USB 调试），数据线连接。
2. ScreenLink 左侧出现设备，点「投屏」。
3. 电脑鼠标/键盘直接操控手机：左键点击拖动、滚轮滚动、右键=返回、中键=Home、Ctrl+V 粘贴（先同步剪贴板）、Esc=返回。
4. 把文件从资源管理器拖进投屏窗口即发送到手机 `Download/ScreenLink/`；工具栏「发送文件/接收」为双向一键传输，进度/速度/剩余时间见右下角传输队列。
5. 剪贴板双向同步默认开启（设置中可关）。

### 无线连接（安卓 11+）

- 侧栏「无线连接 → 扫描」：列出同一 WiFi 下开启了「无线调试」的设备（mDNS）。
- 首次使用：手机「开发者选项 → 无线调试 → 使用配对码配对设备」，把 IP:端口与 6 位配对码填入「配对新设备」。
- 已配对设备直接「连接」。老设备（adb tcpip 5555）可用子网扫描或手动输入 IP。
- USB 在线时点「设置 → 拔线自动切 WiFi」开启后，拔线会自动尝试无线重连。

### 无线连接排错

| 现象 | 处理 |
| --- | --- |
| 扫描不到设备 | 确认手机与电脑同一网段；手机「无线调试」开关重新拨一次（端口会变）；路由器开了 AP 隔离则关闭 |
| 配对码一直失败 | 配对窗口有超时，每次重新生成配对码后再输入；地址用「配对地址」（随机端口）而非「无线调试地址」 |
| 能配对不能连接 | 连接用的是「无线调试」主地址（另一个端口），配对成功后直接点扫描列表里的「连接」 |
| 首次连接掉线 | 部分 ROM 需要先用 USB 连接一次授权 RSA 指纹 |

## MCP 配置指南（AI 一键操控手机）

1. GUI「设置 / AI」→ 打开「允许 AI 操控手机」（生成 token，随时可撤销）。
2. 复制生成的配置片段到 AI 客户端：

```json
{
  "mcpServers": {
    "screenlink": {
      "command": "screenlink-mcp",
      "args": ["--device", "auto", "--token", "<GUI 生成的 token>"]
    }
  }
}
```

3. 远程客户端可用 HTTP 传输：`screenlink-mcp --transport http --port 27191 --token <T>`，端点 `POST http://127.0.0.1:27191/mcp`。

暴露的工具：`screenlink_screenshot` / `screenlink_tap`（支持 page_state 元素 index）/ `screenlink_swipe` / `screenlink_type` / `screenlink_key` / `screenlink_devices` / `screenlink_push_file` / `screenlink_pull_file` / `screenlink_page_state`；资源 `screenlink://device/{serial}/screen` 返回实时截图。

性能路径：GUI 在跑时走 IPC 快速通道（截图复用投屏帧 <60ms、输入复用 Scrcpy 控制 socket <20ms）；GUI 未运行自动退回 adb 慢速通道。所有 AI 操作写入审计日志（GUI「AI 操作审计」面板可视），撤销 token 立即生效。

## CLI 调试

```text
screenlink-cli devices | props | mdns | wifi-scan | connect | pair
screenlink-cli mirror --serial <S> --frames 120      # 收帧统计（验证协议链路）
screenlink-cli screenshot --serial <S> -o shot.png
screenlink-cli push|pull --serial <S> <本地> <远端>
screenlink-cli page --serial <S>                     # uiautomator 页面结构
```

调试环境变量：`SCREENLINK_DEBUG=1` 输出 Scrcpy 握手逐步骤日志；`SCREENLINK_ADB` 指定 adb 路径；`SCREENLINK_SERVER_JAR` 指定 server jar。

## 专题指南

- [无线连接指南](docs/wireless.md)：两种无线模式、配对流程、息屏断连/mDNS 残留等真机踩坑与排查决策树
- [低端手机 / 老电脑适配](docs/low-end.md)：画质预设与参数取舍、WebCodecs 要求、USB 驱动一键修复、MIUI/HyperOS 后台冻结对策

## 已知限制 / 路线图

- 音频转发（Scrcpy Opus 通道协议已预留，UI 未接入）
- 自建 TCP 高速文件通道与断点续传（当前 USB/WiFi 均走 adb push/pull；通道扩展位已预留）
- 手机端发现 agent（UDP 广播响应端）
- Linux/macOS 剪贴板与窗口行为适配（核心已用 arboard，未全量验证）

## 许可

本仓库代码 MIT。`assets/scrcpy-server-v2.7.jar` 来自 [Genymobile/scrcpy](https://github.com/Genymobile/scrcpy)（Apache-2.0）。
