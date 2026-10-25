# ScreenLink 手机伴侣 — Android 设计文档

版本：v1.1（2026-10-25 评审修订：Gradle 测试依赖补全、IRemoteProcess/ServiceCompat API 修正、ProgressHub 溢出策略、init/分享生命周期、无线调试配对流程）
读者：三个并行编码的实现者 + 骨架搭建者。
配套文档：[PROTOCOL.md](./PROTOCOL.md)（PC↔手机 TCP 协议，PC 端 Rust 对接依据）。
本文是文件所有权与架构的唯一事实来源；**实现者只允许创建/修改第 4 节归属表中的文件**。

---

## 1. 环境事实与构建命令（已核实，不要改动）

| 项 | 值 | 来源 |
|---|---|---|
| 工作区 | `D:\screenlink`，应用工程根 `D:\screenlink\android`（当前为空目录） | 本会话 `ls /d/screenlink` 实测 |
| Android SDK | `C:\Users\crisw\AppData\Local\Android\Sdk`（compileSdk 35 / build-tools 35.0.0 已装） | 环境给定 |
| Gradle | 8.9 已在 PATH | 环境给定 |
| 构建用 JVM | Gradle launcher 已是 JDK17 Temurin 17.0.20；**PATH 上的 java 是 26，禁止用于构建** | 环境给定 |
| AGP | 选 **8.7.3**：要求最低 Gradle 8.9（与本机一致），API 35 最低要求 AGP 8.6.0，JDK 17 —— 三条全部满足 | developer.android.com/build/releases/gradle-plugin 版本表；repo1/dl.google.com metadata 确认 8.7.3 存在 |
| Kotlin | **2.0.21**（KSP `2.0.21-1.0.28` 已确认存在于 Maven Central） | repo1.maven.org metadata |
| 模拟器 | `medium_phone`（Android 15 x86_64）= `emulator-5554` 在线，adb 全路径 `"C:\Users\crisw\AppData\Local\Android\Sdk\platform-tools\adb.exe"` | 环境给定 |
| 包名 | `dev.screenlink.companion` | 本文档钦定 |
| Rust 侧端口冲突检查 | `grep -rn "27188" --include="*.rs" --include="*.toml"` 零命中，27188 可安全用作手机监听端口 | 本会话实测 |

构建/安装命令（所有实现者照抄，Git Bash）：

```bash
# 构建前自检：确认 launcher JVM 是 17（不应出现 26）
gradle --version | grep -E "Launcher|Gradle"

cd /d/screenlink/android && gradle :app:assembleDebug --console=plain

# 装到模拟器
"C:\Users\crisw\AppData\Local\Android\Sdk\platform-tools\adb.exe" -s emulator-5554 install -r \
  /d/screenlink/android/app/build/outputs/apk/debug/app-debug.apk

# Room DAO 仪器化测试
cd /d/screenlink/android && gradle :app:connectedDebugAndroidTest
```

注意：不使用 Gradle Wrapper（三台实现设备共用本机 PATH 上的 Gradle 8.9）；不要在 `gradle.properties` 里设置 `org.gradle.java.home`（launcher 已是 JDK17，Kotlin/Worker 守护进程继承同一 JVM，PATH 上的 java 26 不会被触碰）。

---

## 2. 工程结构总览

```
android/
├─ settings.gradle.kts                         [骨架]
├─ build.gradle.kts                            [骨架]
├─ gradle.properties                           [骨架]
├─ local.properties                            [骨架；sdk.dir=C:\\Users\\crisw\\AppData\\Local\\Android\\Sdk；gitignore]
├─ gradle/libs.versions.toml                   [骨架]
└─ app/
   ├─ build.gradle.kts                         [骨架]
   ├─ proguard-rules.pro                       [骨架]
   ├─ schemas/                                 [KSP 自动生成，Room schema 导出目录，入 git]
   └─ src/
      ├─ main/
      │  ├─ AndroidManifest.xml                [骨架]
      │  ├─ java/dev/screenlink/companion/
      │  │  ├─ ScreenLinkApp.kt                [骨架]
      │  │  ├─ MainActivity.kt                 [骨架]
      │  │  ├─ core/  net/  service/  transfer/    [实现者 A]
      │  │  ├─ data/  saf/                          [实现者 B]
      │  │  ├─ ui/                                  [实现者 C]
      │  │  │  └─ theme/Theme.kt                    [骨架]
      │  │  ├─ share/  shizuku/                     [实现者 C]
      ├─ res/values/strings.xml                     [骨架]（公共键，前缀 sl_）
      ├─ res/values/strings_service.xml             [实现者 A]（前缀 sl_svc_）
      ├─ res/values/strings_ui.xml                  [实现者 C]（前缀 sl_ui_）
      ├─ res/values/themes.xml、colors.xml           [骨架]（Theme.ScreenLink、Theme.ScreenLink.Dialog）
      ├─ res/drawable/ic_stat_screenlink.xml        [骨架]（通知小图标）
      ├─ res/mipmap-*                               [骨架]
      │  └─ assets/scrcpy-server                    [实现者 C 放置，scrcpy-server v3.1 jar]
      ├─ test/java/dev/screenlink/companion/        [单测，归属跟随同包实现者]
      └─ androidTest/java/dev/screenlink/companion/data/ [实现者 B]
```

### 共享资源防冲突规则

- **骨架统一提供**：manifest、gradle 全套、theme、`strings.xml`、图标、`ScreenLinkApp.kt`、`MainActivity.kt`、`ui/theme/Theme.kt`。骨架文件在三个实现者动工前合入，此后实现者**不得修改**；需要新的公共字符串时写进**自己的** strings 文件（Android 资源合并允许多个 strings 文件并存，这正是避免并行冲突的机制）。
- 每个实现者的新 `.kt` 文件只能落在第 4 节表中列出的路径；新增文件需先在模块群里声明，避免撞包名下的同名文件。
- 字符串键前缀强制：骨架 `sl_`、A `sl_svc_`、C `sl_ui_`。

---

## 3. 骨架提供物清单（含精确内容要求）

骨架文件内容在本文档第 5、6 节中逐字给出（manifest、gradle 四件套），骨架照抄即可。其余骨架文件要求：

1. `ScreenLinkApp.kt`：`class ScreenLinkApp : Application()`，空实现（不引入 DI 框架，各模块手动构造依赖）。
2. `MainActivity.kt`：`ComponentActivity` + `setContent { ScreenLinkTheme { RootScreen() } }`。**契约**：它调用 `dev.screenlink.companion.ui.RootScreen`（C 必须提供无参 `@Composable fun RootScreen()`，见第 12 节）。
3. `ui/theme/Theme.kt`：`@Composable fun ScreenLinkTheme(content: @Composable () -> Unit)`，Material3 浅色主题。
4. `themes.xml`：`Theme.ScreenLink`（MainActivity 用）与 `Theme.ScreenLink.Dialog`（分享透明对话框用，`windowIsFloating=true`）。
5. `strings.xml` 公共键：`sl_app_name`（"ScreenLink 伴侣"）、`sl_share_send_to_pc`（"发送到电脑"，manifest 引用）。
6. `drawable/ic_stat_screenlink.xml`：白色单色通知图标。

---

## 4. 模块划分与文件所有权清单（核心）

三个实现者并行编码，**所有权零重叠**。依赖方向单向：`C → A、B；A → B；B 不依赖任何人`。A、B 禁止 import C 的任何类；B 禁止 import A、C。

### 实现者 A — 传输内核（协议、服务、传输引擎）

基础路径：`app/src/main/java/dev/screenlink/companion/`

| # | 文件 | 职责 |
|---|---|---|
| A1 | `core/Protocol.kt` | 协议常量唯一来源：`PORT=27188`、魔数 `SLNK`、版本 1、帧类型枚举（对应 PROTOCOL.md §4）、payload 上限、超时常量 |
| A2 | `core/FrameCodec.kt` | 12 字节帧头编解码 + JSON payload 序列化（kotlinx.serialization 或 org.json 手写均可，二进制一致即可）；magic/version 校验 |
| A3 | `net/PairingStore.kt` | token 持久化：SharedPreferences（`MODE_PRIVATE`、`allowBackup=false` 已生效）；存 `token_id → sha256(token)` 映射与设备随机实例 id（8 hex）；**永不存明文 token**（与 PROTOCOL.md §3.1 一致——PC 侧才持有明文） |
| A4 | `net/PairingCoordinator.kt` | 配对码生成/过期/尝试次数（PROTOCOL.md §3 规则）；`HKDF-SHA256` token 派生与校验（`MessageDigest.isEqual` 常量时间比较） |
| A5 | `net/AgentServer.kt` | `object`。`ServerSocket(27188)` accept 循环、每连接状态机（HELLO→AUTH/PAIR→命令分发）、单 PC 会话管理、限速/冷却、私网地址过滤 |
| A6 | `service/AgentService.kt` | 前台服务薄壳：`startForeground(TYPE_SPECIAL_USE)`、启停 `AgentServer`、持有通知更新；companion 提供 `start(ctx)/stop(ctx)` |
| A7 | `service/AgentNotifications.kt` | 通知渠道（`sl_agent` 常驻、`sl_transfer` 进度）、进度通知 250ms 节流刷新、停止 Action |
| A8 | `transfer/TransferEngine.kt` | `object`。xid 分配、并发上限（8）、SEND/RECV 任务调度、`TransferGateway` 门面：`suspend fun submitShares(uris: List<Uri>): SubmitResult` |
| A9 | `transfer/SendTask.kt` | 发送方向：流式读 Uri → DATA 帧（≤1MiB/帧），支持 offset 续传（skip 前缀）、写历史进度 |
| A10 | `transfer/ReceiveTask.kt` | 接收方向：DATA → SAF 写入（B 的 `SafWriter`），续传前 prefix re-hash + 滚动 MD5，结束比对/DONE 帧 |
| A11 | `transfer/ProgressHub.kt` | `object`。`val events: MutableSharedFlow<ProgressEvent>(replay=0, extraBufferCapacity=64, onBufferOverflow=BufferOverflow.DROP_OLDEST)`，引擎→通知→UI 的进度总线。**必须显式 DROP_OLDEST**：默认 SUSPEND 会在无收集者时把 `emit` 永久挂起、卡死传输引擎；事件为 lossy（仅展示用），订阅方不得假设逐帧必达，终态展示以 Room 历史状态为最终事实来源 |
| A12 | `transfer/Digests.kt` | 流式 MD5 工具（`MessageDigest` 包装，`fun md5Of(stream): String`、增量器类） |

测试（同归 A）：`app/src/test/java/dev/screenlink/companion/core/FrameCodecTest.kt`、`transfer/DigestsTest.kt`（纯 JVM JUnit）。

资源（同归 A）：`app/src/main/res/values/strings_service.xml`。

**A 对外契约（其他模块只允许按这些签名调用）：**

```kotlin
// AgentServer.kt
sealed interface AgentState {
    data object Idle : AgentState
    data class Listening(val ip: String?, val port: Int) : AgentState
    data class AwaitingPairing(val ip: String?, val code: String, val expiresAtEpochMs: Long) : AgentState
    data class Paired(val ip: String?, val peerName: String) : AgentState
    data class Failed(val reason: String) : AgentState
}
object AgentServer {
    val state: StateFlow<AgentState>
    fun start(context: Context)   // 由 AgentService 调；内部拉起 mDNS 注册（NsdManager）
    fun stop(context: Context)
}

// ProgressHub.kt
sealed interface ProgressEvent {
    data class Started(val recordId: Long, val xid: Int, val direction: Int, val fileName: String, val total: Long) : ProgressEvent
    data class Progress(val recordId: Long, val transferred: Long, val total: Long) : ProgressEvent
    data class Finished(val recordId: Long, val ok: Boolean, val md5: String?) : ProgressEvent
}
object ProgressHub {
    // extraBufferCapacity=64 且 onBufferOverflow=BufferOverflow.DROP_OLDEST（lossy，emit/tryEmit 永不挂起）；
    // 无收集者（UI 已销毁、通知未就绪）时事件被丢弃而非阻塞引擎
    val events: MutableSharedFlow<ProgressEvent>
}

// TransferEngine.kt
sealed interface SubmitResult {
    data class Queued(val recordIds: List<Long>) : SubmitResult              // PC 未连接：入 outbox
    data class Sent(val recordIds: List<Long>) : SubmitResult                // 已连接：立即发送
    data class NeedPairing(val recordIds: List<Long>) : SubmitResult         // 已监听但未配对：入 outbox + 显示配对码
}
object TransferEngine {
    fun init(appContext: Context)          // 幂等（重复调用无副作用）。调用责任人：AgentService.onCreate 与 SendToPcActivity.onCreate（在 submitShares 之前）——后者覆盖"新装 App 未启动过服务、直接系统分享"路径，见第 11 节
    suspend fun submitShares(uris: List<Uri>): SubmitResult   // C 的分享页唯一入口；若 init 从未被调用须抛 IllegalStateException 快速失败（而非静默打到未初始化引擎）
}
```

### 实现者 B — 存储与历史（Room + SAF）

| # | 文件 | 职责 |
|---|---|---|
| B1 | `data/TransferRecord.kt` | Room 实体（表结构见第 7 节，逐字实现） |
| B2 | `data/TransferDao.kt` | DAO（第 7 节逐字实现） |
| B3 | `data/AppDatabase.kt` | `abstract class AppDatabase : RoomDatabase()`；`companion fun get(context): AppDatabase` 单例，库名 `screenlink.db`，version 1 |
| B4 | `data/TransferRepository.kt` | DAO 薄封装（**无状态**，A、C 可各自实例化）；状态常量 `STATUS_RUNNING/DONE/FAILED/CANCELLED/QUEUED` 与 `DIR_RECV/DIR_SEND` 定义处 |
| B5 | `saf/SafStore.kt` | tree Uri 的保存/恢复/校验：`takePersistableUriPermission`、`persistedUriPermissions` 失效检测、prefs 键 `saf_tree_uri` |
| B6 | `saf/SafWriter.kt` | SAF 写入器：建文档（冲突自动改名+回查真名）、`"w"/"wa"` 输出流、按名查找半成品（续传）、删除、size 查询 |

测试（同归 B）：`app/src/androidTest/java/dev/screenlink/companion/data/HistoryDaoTest.kt`（Room 仪器化测试，跑在 emulator-5554）。

**B 对外契约：**

```kotlin
// TransferRepository.kt
class TransferRepository(private val dao: TransferDao, context: Context) {
    fun observeRecent(limit: Int = 200): Flow<List<TransferRecord>>
    suspend fun enqueue(record: TransferRecord): Long
    suspend fun updateProgress(id: Long, transferred: Long)
    suspend fun finish(id: Long, status: Int, md5: String?, finishedAt: Long, transferred: Long)
    suspend fun queuedForPc(): List<TransferRecord>                       // direction=SEND, status=QUEUED（LIST 命令数据源）
    suspend fun findResumable(fileName: String): TransferRecord?          // direction=RECV, status IN (RUNNING, FAILED)，倒序取 1
    suspend fun failAllRunning(now: Long)                                 // 服务启动时把上次 RUNNING 收尸为 FAILED
    suspend fun clear()
}

// SafWriter.kt
class SafWriter(private val store: SafStore, private val resolver: ContentResolver) {
    fun hasValidTree(): Boolean
    fun createUnique(displayName: String, mime: String): Uri              // createDocument；提供方可能自动改名，必须回查 DISPLAY_NAME
    fun openOverwrite(uri: Uri): OutputStream                             // openOutputStream(uri, "w")
    fun openAppend(uri: Uri): OutputStream                                // openOutputStream(uri, "wa")（续传追加）
    fun sizeOf(uri: Uri): Long
    fun findByName(displayName: String): Pair<Uri, Long>?                 // (uri, 已有字节数)——断点续传查找
    fun delete(uri: Uri)
}
```

### 实现者 C — UI 与 Shizuku（Compose、分享入口、提权模块）

| # | 文件 | 职责 |
|---|---|---|
| C1 | `ui/RootScreen.kt` | **骨架契约**：`@Composable fun RootScreen()`（无参）；三段式底部导航：状态/历史/设置 |
| C2 | `ui/StatusPanel.kt` | 监听状态卡（IP:端口、AgentState）、配对码大字显示+倒计时、启停 AgentService、`POST_NOTIFICATIONS` 运行时请求 |
| C3 | `ui/HistoryPanel.kt` | 历史列表（`TransferRepository.observeRecent` 收 Flow）、清空按钮 |
| C4 | `ui/SettingsPanel.kt` | SAF 选目录入口（`ActivityResultContracts.OpenDocumentTree`）、Shizuku 卡片（安装检测/授权/开无线调试/启动 scrcpy） |
| C5 | `ui/MainViewModel.kt` | 装配 B 的 Repository + A 的 AgentServer.state/ProgressHub，转发给三个 Panel |
| C6 | `share/SendToPcActivity.kt` | 分享目标 Activity：解析 `SEND/SEND_MULTIPLE`、先调 `TransferEngine.init(applicationContext)`（幂等）再 `submitShares`；**每 intent 只提交一次**（提交标志与结果存 ViewModel，旋转/分屏重建后只恢复进度展示、不重新提交，见第 11 节）；进度对话框、完成后 finish（**不提前 finish**，规避分享 URI 授权随 Activity 销毁失效，见第 11 节） |
| C7 | `shizuku/ShizukuGate.kt` | `Shizuku.pingBinder()` 可用性、授权请求与结果监听 |
| C8 | `shizuku/ShellRunner.kt` | `IShizukuService.newProcess` 封装（返回 `IRemoteProcess`：`getInputStream()`=stdout、`getOutputStream()`=stdin、`waitFor()`）；`sh -c "<cmd>; echo __EXIT__=$?"` 模式取退出码与输出 |
| C9 | `shizuku/WirelessDebugEnabler.kt` | shell 开无线调试 + 两阶段 mDNS：`_adb-tls-pairing._tcp`（配对端口，配对对话框存续期间才广播，引导 PC `adb pair` + 6 位码）与 `_adb-tls-connect._tcp`（连接端口），展示 `ip:port` |
| C10 | `shizuku/ScrcpyLauncher.kt` | 推送 assets/scrcpy-server 到 `/data/local/tmp`（经 `IRemoteProcess` stdin 写入）、`app_process` 以 shell 启动 scrcpy server、监听 stdout 判断就绪 |

资源（同归 C）：`app/src/main/res/values/strings_ui.xml`；资产：`app/src/main/assets/scrcpy-server`（C 负责下载 scrcpy-server v3.1 jar 放入，提交说明附 sha256）。

---

## 5. AndroidManifest 全组件声明（骨架逐字采用）

```xml
<?xml version="1.0" encoding="utf-8"?>
<manifest xmlns:android="http://schemas.android.com/apk/res/android">

    <uses-permission android:name="android.permission.INTERNET" />
    <uses-permission android:name="android.permission.ACCESS_NETWORK_STATE" />
    <uses-permission android:name="android.permission.ACCESS_WIFI_STATE" />
    <uses-permission android:name="android.permission.FOREGROUND_SERVICE" />
    <uses-permission android:name="android.permission.FOREGROUND_SERVICE_SPECIAL_USE" />
    <uses-permission android:name="android.permission.POST_NOTIFICATIONS" />

    <application
        android:name=".ScreenLinkApp"
        android:allowBackup="false"
        android:icon="@mipmap/ic_launcher"
        android:label="@string/sl_app_name"
        android:theme="@style/Theme.ScreenLink">

        <!-- 启动页 -->
        <activity
            android:name=".MainActivity"
            android:exported="true">
            <intent-filter>
                <action android:name="android.intent.action.MAIN" />
                <category android:name="android.intent.category.LAUNCHER" />
            </intent-filter>
        </activity>

        <!-- 系统分享「发送到电脑」：SEND / SEND_MULTIPLE，任意 MIME -->
        <activity
            android:name=".share.SendToPcActivity"
            android:exported="true"
            android:excludeFromRecents="true"
            android:label="@string/sl_share_send_to_pc"
            android:theme="@style/Theme.ScreenLink.Dialog">
            <intent-filter>
                <action android:name="android.intent.action.SEND" />
                <category android:name="android.intent.category.DEFAULT" />
                <data android:mimeType="*/*" />
            </intent-filter>
            <intent-filter>
                <action android:name="android.intent.action.SEND_MULTIPLE" />
                <category android:name="android.intent.category.DEFAULT" />
                <data android:mimeType="*/*" />
            </intent-filter>
        </activity>

        <!-- 前台 AgentService：TCP 监听 27188 + 传输进度通知 -->
        <service
            android:name=".service.AgentService"
            android:exported="false"
            android:foregroundServiceType="specialUse">
            <property
                android:name="android.app.PROPERTY_SPECIAL_USE_FGS_SUBTYPE"
                android:value="Local PC companion link: keeps a TCP listener on port 27188 for paired-PC file transfer and shows transfer progress notifications" />
        </service>

        <!-- Shizuku 提权 Provider（官方固定写法） -->
        <provider
            android:name="rikka.shizuku.ShizukuProvider"
            android:authorities="${applicationId}.shizuku"
            android:multiprocess="false"
            android:enabled="true"
            android:exported="true"
            android:permission="android.permission.INTERACT_ACROSS_USERS_FULL" />
    </application>

    <!-- Android 11+ 包可见性：Shizuku 管理器 -->
    <queries>
        <package android:name="moe.shizuku.privileged.api" />
    </queries>
</manifest>
```

**为什么 `specialUse` 而不是 `dataSync`（决策 D1）**：Android 15（targetSdk 35 命中）引入 dataSync 前台服务 **24 小时窗口内累计 6 小时**运行上限，超时走 `Service.onTimeout`，不 `stopSelf` 直接 `RemoteServiceException` 崩溃；超时后甚至不允许再启动（`ForegroundServiceStartNotAllowedException`）。常驻 TCP 监听服务必然触雷。`specialUse` 无运行时长限制，要求 = `FOREGROUND_SERVICE_SPECIAL_USE` 权限 + `<property android:name="android.app.PROPERTY_SPECIAL_USE_FGS_SUBTYPE">`（无运行时前置条件）。证据：developer.android.com/about/versions/15/behavior-changes-15 与 /develop/background-work/services/fg-service-types（本会话已抓取原文）。
副作用说明：`allowBackup=false` 同时保证 A3 的 token 持久化不进云备份；明文 TCP 不受 `usesCleartextTraffic` 约束（该开关只管 HTTP 栈），无需声明。

---

## 6. Gradle 配置（骨架逐字采用）

### 6.1 `android/gradle/libs.versions.toml`

```toml
[versions]
agp = "8.7.3"
kotlin = "2.0.21"
ksp = "2.0.21-1.0.28"
composeBom = "2024.10.01"
coreKtx = "1.13.1"
activityCompose = "1.9.3"
lifecycle = "2.8.7"
coroutines = "1.9.0"
room = "2.6.1"
shizuku = "13.1.5"
junit = "4.13.2"
androidxTestExt = "1.2.1"
androidxTestRunner = "1.6.2"

[libraries]
androidx-core-ktx = { group = "androidx.core", name = "core-ktx", version.ref = "coreKtx" }
androidx-activity-compose = { group = "androidx.activity", name = "activity-compose", version.ref = "activityCompose" }
androidx-lifecycle-runtime-ktx = { group = "androidx.lifecycle", name = "lifecycle-runtime-ktx", version.ref = "lifecycle" }
androidx-lifecycle-viewmodel-compose = { group = "androidx.lifecycle", name = "lifecycle-viewmodel-compose", version.ref = "lifecycle" }
compose-bom = { group = "androidx.compose", name = "compose-bom", version.ref = "composeBom" }
compose-ui = { group = "androidx.compose.ui", name = "ui" }
compose-ui-tooling-preview = { group = "androidx.compose.ui", name = "ui-tooling-preview" }
compose-ui-tooling = { group = "androidx.compose.ui", name = "ui-tooling" }
compose-material3 = { group = "androidx.compose.material3", name = "material3" }
compose-material-icons = { group = "androidx.compose.material", name = "material-icons-extended" }
coroutines-android = { group = "org.jetbrains.kotlinx", name = "kotlinx-coroutines-android", version.ref = "coroutines" }
coroutines-test = { group = "org.jetbrains.kotlinx", name = "kotlinx-coroutines-test", version.ref = "coroutines" }
room-runtime = { group = "androidx.room", name = "room-runtime", version.ref = "room" }
room-ktx = { group = "androidx.room", name = "room-ktx", version.ref = "room" }
room-compiler = { group = "androidx.room", name = "room-compiler", version.ref = "room" }
shizuku-api = { group = "dev.rikka.shizuku", name = "api", version.ref = "shizuku" }
shizuku-provider = { group = "dev.rikka.shizuku", name = "provider", version.ref = "shizuku" }
junit = { group = "junit", name = "junit", version.ref = "junit" }
androidx-test-ext-junit = { group = "androidx.test.ext", name = "junit", version.ref = "androidxTestExt" }
androidx-test-runner = { group = "androidx.test", name = "runner", version.ref = "androidxTestRunner" }
room-testing = { group = "androidx.room", name = "room-testing", version.ref = "room" }

[plugins]
android-application = { id = "com.android.application", version.ref = "agp" }
kotlin-android = { id = "org.jetbrains.kotlin.android", version.ref = "kotlin" }
kotlin-compose = { id = "org.jetbrains.kotlin.plugin.compose", version.ref = "kotlin" }
ksp = { id = "com.google.devtools.ksp", version.ref = "ksp" }
```

全部版本号已在本会话通过 Maven metadata 核实存在：AGP 8.7.3（dl.google.com）、KSP 2.0.21-1.0.28、Shizuku api 13.1.5（repo1.maven.org，且为 latest）、Compose BOM 2024.10.01、Room 2.6.1 与 room-testing 2.6.1（dl.google.com metadata 实查）、androidx.test:runner 1.6.2（dl.google.com metadata 实查）。Kotlin 2.0.21 由 KSP 版本号前缀 `2.0.21-` 反向锁定。Shizuku 在 Maven Central（`dev.rikka.shizuku`），无需额外仓库。

### 6.2 `android/settings.gradle.kts`

```kotlin
pluginManagement {
    repositories {
        google()
        mavenCentral()
        gradlePluginPortal()
    }
}
dependencyResolutionManagement {
    repositoriesMode.set(RepositoriesMode.FAIL_ON_PROJECT_REPOS)
    repositories {
        google()
        mavenCentral()
    }
}
rootProject.name = "ScreenLink-Companion"
include(":app")
```

### 6.3 `android/build.gradle.kts`（根）

```kotlin
plugins {
    alias(libs.plugins.android.application) apply false
    alias(libs.plugins.kotlin.android) apply false
    alias(libs.plugins.kotlin.compose) apply false
    alias(libs.plugins.ksp) apply false
}
```

### 6.4 `android/gradle.properties`

```properties
org.gradle.jvmargs=-Xmx2048m -Dfile.encoding=UTF-8
android.useAndroidX=true
android.nonTransitiveRClass=true
kotlin.code.style=official
```

### 6.5 `android/app/build.gradle.kts`

```kotlin
plugins {
    alias(libs.plugins.android.application)
    alias(libs.plugins.kotlin.android)
    alias(libs.plugins.kotlin.compose)   // Kotlin 2.0 起 Compose 编译器独立插件（随 Kotlin 版本 2.0.21）
    alias(libs.plugins.ksp)
}

android {
    namespace = "dev.screenlink.companion"
    compileSdk = 35

    defaultConfig {
        applicationId = "dev.screenlink.companion"
        minSdk = 26
        targetSdk = 35
        versionCode = 1
        versionName = "0.1.0"
        testInstrumentationRunner = "androidx.test.runner.AndroidJUnitRunner"
    }

    buildTypes {
        release {
            isMinifyEnabled = true
            proguardFiles(getDefaultProguardFile("proguard-android-optimize.txt"), "proguard-rules.pro")
        }
    }
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
    kotlinOptions { jvmTarget = "17" }
    buildFeatures { compose = true }
}

ksp {
    arg("room.schemaLocation", "$projectDir/schemas")   // Room schema 导出，入 git
}

dependencies {
    implementation(libs.androidx.core.ktx)
    implementation(libs.androidx.activity.compose)
    implementation(libs.androidx.lifecycle.runtime.ktx)
    implementation(libs.androidx.lifecycle.viewmodel.compose)

    implementation(platform(libs.compose.bom))
    implementation(libs.compose.ui)
    implementation(libs.compose.material3)
    implementation(libs.compose.material.icons)
    implementation(libs.compose.ui.tooling.preview)
    debugImplementation(libs.compose.ui.tooling)

    implementation(libs.coroutines.android)

    implementation(libs.room.runtime)
    implementation(libs.room.ktx)
    ksp(libs.room.compiler)

    implementation(libs.shizuku.api)
    implementation(libs.shizuku.provider)

    testImplementation(libs.junit)
    testImplementation(libs.coroutines.test)
    androidTestImplementation(libs.androidx.test.ext.junit)
    androidTestImplementation(libs.androidx.test.runner)   // testInstrumentationRunner 指向的 AndroidJUnitRunner 在此 artifact；已核实 ext-junit 1.2.1 / androidx.test:core 1.6.1 的 POM 均不传递它
    androidTestImplementation(libs.room.testing)
    androidTestImplementation(libs.coroutines.test)
}
```

---

## 7. Room 表结构：传输历史（B 逐字实现）

单表 `transfer_history`，**兼作断点续传状态源**（决策 D4）：`status=RUNNING/FAILED` 的行 + SAF 半成品文档共同描述一次未完成接收，服务启动时收尸，无需第二张续传表。

```kotlin
// data/TransferRecord.kt
@Entity(
    tableName = "transfer_history",
    indices = [Index("status"), Index("startedAt")]
)
data class TransferRecord(
    @PrimaryKey(autoGenerate = true) val id: Long = 0,
    val direction: Int,          // 0 = RECV（PC→手机），1 = SEND（手机→PC）
    val fileName: String,        // SAF 建档后回查的"真实"显示名（可能被自动改名）
    val sizeBytes: Long,
    val mimeType: String?,
    val peerName: String,        // 对端主机名（HELLO.name）
    val peerHost: String,        // 对端 IP
    val status: Int,             // 0=RUNNING 1=DONE 2=FAILED 3=CANCELLED 4=QUEUED(离线 outbox)
    val transferredBytes: Long,  // 已传输字节；QUEUED 行恒为 0
    val md5: String?,            // 完整文件 MD5（32 位小写 hex）；完成前可空
    val startedAt: Long,         // epoch ms
    val finishedAt: Long?,       // epoch ms，未完成为 null
    val contentUri: String?,     // RECV=SAF document uri；SEND=分享源 uri 或 outbox 缓存文件路径
)
```

```kotlin
// data/TransferDao.kt
@Dao
interface TransferDao {
    @Query("SELECT * FROM transfer_history ORDER BY startedAt DESC LIMIT :limit")
    fun observeRecent(limit: Int = 200): Flow<List<TransferRecord>>

    @Insert
    suspend fun insert(r: TransferRecord): Long

    @Query("UPDATE transfer_history SET transferredBytes = :bytes WHERE id = :id")
    suspend fun updateProgress(id: Long, bytes: Long)

    @Query("UPDATE transfer_history SET status = :status, md5 = :md5, finishedAt = :finishedAt, transferredBytes = :transferred WHERE id = :id")
    suspend fun finish(id: Long, status: Int, md5: String?, finishedAt: Long, transferred: Long)

    @Query("SELECT * FROM transfer_history WHERE direction = 1 AND status = 4 ORDER BY startedAt")
    suspend fun queued(): List<TransferRecord>

    @Query("SELECT * FROM transfer_history WHERE direction = 0 AND status IN (0, 2) AND fileName = :fileName ORDER BY startedAt DESC LIMIT 1")
    suspend fun findResumable(fileName: String): TransferRecord?

    @Query("UPDATE transfer_history SET status = 2, finishedAt = :now WHERE status = 0")
    suspend fun failAllRunning(now: Long)

    @Query("DELETE FROM transfer_history")
    suspend fun clear()
}
```

```kotlin
// data/AppDatabase.kt（要点）
@Database(entities = [TransferRecord::class], version = 1, exportSchema = true)
abstract class AppDatabase : RoomDatabase() {
    abstract fun transferDao(): TransferDao
    companion object {
        @Volatile private var inst: AppDatabase? = null
        fun get(context: Context): AppDatabase = inst ?: synchronized(this) {
            inst ?: Room.databaseBuilder(context.applicationContext, AppDatabase::class.java, "screenlink.db")
                .build().also { inst = it }
        }
    }
}
```

版本策略：v1 无迁移；后续加列必须 bump version 并写 `Migration`，`schemas/` 目录随 git 留痕。

---

## 8. SAF 目录授权流程（B 实现 SafStore/SafWriter，C 在 SettingsPanel 触发）

1. **发起选择**（C）：`rememberLauncherForActivityResult(ActivityResultContracts.OpenDocumentTree()) { uri -> vm.onTreePicked(uri) }`，`launch(null)`。
2. **持久化授权**（B `SafStore.save`）：
   `contentResolver.takePersistableUriPermission(uri, Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_GRANT_WRITE_URI_PERMISSION)` —— 授权跨重启存活；随后 prefs 写 `saf_tree_uri = uri.toString()`。
3. **有效性校验**（B `SafStore.current()`）：启动时读 prefs，比对 `contentResolver.persistedUriPermissions` 中是否存在该 uri 且 `isWritePermission`；再对树根执行一次子文档 query 探活（用户可能在系统设置里撤销）。任一不满足 → 返回 null，UI 提示重新授权。
4. **接收写入**（B `SafWriter.createUnique`）：`DocumentsContract.createDocument(resolver, treeUri, mime ?: "application/octet-stream", displayName)`。提供方撞名时会自动改成 `name (1).ext`，**必须回查返回文档的 `DISPLAY_NAME`** 并把真名写进历史行（`fileName`）。
5. **全新写 / 续传写**：`openOutputStream(uri, "w")` 全新；`openOutputStream(uri, "wa")` 追加（FileSystemProvider 支持 `wa`，续传依赖此模式）。
6. **续传查找**（B `SafWriter.findByName`）：遍历树的子文档，匹配 `DISPLAY_NAME == fileName` 且 `COLUMN_SIZE` ≤ 期望大小 → 返回 `(uri, size)` 作为 ACK offset 依据；配合 DAO `findResumable`（status RUNNING/FAILED 的行）双重确认。
7. **空间预检**：尝试对树根 query `available_bytes` 列，查得到则与 `sizeBytes` 比较，不足 → SEND_ACK `reason=NO_STORAGE`；查不到（非标准提供方）跳过预检，靠写盘 IO 错误兜底。
8. **失败清理**：传输 CANCEL 且 `keep=false`、或最终 MD5 不匹配 → `DocumentsContract.deleteDocument`；应用删除历史时仅删行，不动用户目录里的文件。

---

## 9. AgentService 前台服务与通知（A）

- 生命周期：`AgentService.start(ctx)` → `startForegroundService` → `onCreate` 调 `TransferEngine.init()` + `AgentServer.start()`，**`ServiceCompat.startForeground(this, NOTIF_ID, build(), ServiceInfo.FOREGROUND_SERVICE_TYPE_SPECIAL_USE)`**；`stop` 反向。服务被系统回收后由用户在 UI 重新启动（不实现开机自启，超出本期范围）。
  - **禁止直接调用三参 `startForeground(id, notif, type)` 重载**：该重载 API 29+ 才存在，本项目 minSdk=26，API 26-28 真机即 `NoSuchMethodError`。`ServiceCompat.startForeground`（androidx.core 1.13.1，已核实其源码按 SDK_INT 分支：≥34 传完整 type / 29-33 传 type / <29 自动退化为两参）是唯一允许写法；`FOREGROUND_SERVICE_TYPE_SPECIAL_USE` 常量在低版本设备上由 compat 层忽略，编译期内联无运行时依赖。
- `onCreate` 时调 `TransferRepository.failAllRunning(now)` 收尸上次未完成接收，半成品 SAF 文档保留供续传。
- 通知：
  - 渠道 `sl_agent`（IMPORTANCE_LOW）：常驻"ScreenLink 正在监听 192.168.x.x:27188"，附"停止"Action（PendingIntent → Service `ACTION_STOP`）。
  - 渠道 `sl_transfer`（IMPORTANCE_LOW）：传输进行中显示进度条，订阅 `ProgressHub.events`，**250ms 节流**刷新；完成/失败发一次即清除。
- 状态流：`AgentServer.state: StateFlow<AgentState>`（C 的 StatusPanel 收集渲染）；`AwaitingPairing` 携带配对码与过期时间戳。

## 10. Shizuku 可选模块（C）

- **可用性**（C7）：`Shizuku.pingBinder()`（`queries` 已声明包可见性）→ false 则卡片显示"未安装/未运行 Shizuku"。
- **授权**（C7）：`Shizuku.requestPermission(REQ_CODE)` + `addRequestPermissionResultListener`；已授权状态用 `Shizuku.checkSelfPermission()`。
- **Shell**（C8）：`IShizukuService.Stub.asInterface(Shizuku.getBinder()).newProcess(cmd, env, dir)` **返回 `moe.shizuku.server.IRemoteProcess`（不是 ParcelFileDescriptor）**——已核实 Shizuku api 13.1.5 源码（`ShizukuRemoteProcess` 包装该类型）：`getInputStream()`=远端进程 stdout、`getOutputStream()`=远端进程 **stdin**、`waitFor()`/`exitValue()`/`destroy()`。读取 stdout 用 `getInputStream()` 解析；统一包装 `sh -c "<cmd>; echo __EXIT__=$?"` 从 stdout 尾部解析退出码。
- **自开无线调试 + 配对引导**（C9）：无线调试在 Android 11+ 由密钥配对保护，**只开开关不够，PC 首次使用必须先 `adb pair`**，流程分两阶段：
  1. 开启：`settings put global adb_wifi_enabled 1`；回读 `settings get global adb_wifi_enabled` 校验。
  2. **配对（首次，一次即可）**：用户在系统"开发者选项 → 无线调试 → 使用配对码配对设备"打开配对对话框——**该对话框存续期间**手机才广播 `_adb-tls-pairing._tcp`（含配对端口），C9 经 `NsdManager.discoverServices("_adb-tls-pairing._tcp.", NSD_PROTOCOL_DNS_SD, listener)` 取得配对端口并展示 `<ip>:<配对端口>` + 对话框上的 6 位配对码；PC 侧执行 `adb pair <ip>:<配对端口>`（交互输入码，或 `adb pair <ip>:<port> <code>`）。配对成功后密钥持久，此后无需重复。
  3. 连接：`NsdManager.discoverServices("_adb-tls-connect._tcp.", NSD_PROTOCOL_DNS_SD, listener)` 取连接端口，UI 展示 `<ip>:<port>` 供 PC `adb connect`。
- **以 shell 启动 scrcpy server**（C10）：
  1. 把 `assets/scrcpy-server`（v3.1 jar）经 shell 写入 `/data/local/tmp/scrcpy-server`：`newProcess(["sh","-c","cat > /data/local/tmp/scrcpy-server"], …)` 后**向返回的 `IRemoteProcess.getOutputStream()`（即进程 stdin）写入 jar 字节并关闭输出流**，再从 `getInputStream()` 读 `__EXIT__` 校验成功。
  2. 启动命令（env 携带 CLASSPATH，与 scrcpy 官方 adb-shell 启动方式一致）：
     `env=["CLASSPATH=/data/local/tmp/scrcpy-server"]`，cmd=`["app_process", "/", "com.genymobile.scrcpy.Server", "3.1", "log_level=info", "stay_awake=true"]`
     —— 默认 tunnel 模式监听 `localabstract:scrcpy`。
  3. UI 展示 PC 侧配方（首次使用须先完成 C9 的 `adb pair` 配对）：
     `adb pair <ip>:<配对端口>`（仅首次）→ `adb connect <ip>:<无线调试端口>` → `adb forward tcp:27183 localabstract:scrcpy` → `scrcpy -s <ip>:<端口> --force-adb-forward`。
  4. server 在 PC 断开时自退出；C10 监听 stdout EOF 恢复空闲态。

## 11. 系统分享「发送到电脑」链路（C6 + A）

1. 系统分享 → `SendToPcActivity`（single 次任务、透明对话框主题），从 intent 取 `EXTRA_STREAM`（单个）/`EXTRA_STREAM` 列表（多个）。
2. **初始化**：Activity `onCreate` 先调 `TransferEngine.init(applicationContext)`（幂等，A 保证重复调用无副作用）。这覆盖"新装 App 后服务从未启动、直接系统分享"的路径——`init` 不只由 `AgentService.onCreate` 触发。
3. **每 intent 只提交一次**：intent 解析与 `submitShares` 调用放在 ViewModel 域内（`onCreate` 时若 ViewModel 已持有当前 intent 的提交结果则直接复用）。旋转/分屏导致 Activity 重建、`onCreate`/`onNewIntent` 重新执行时，**只重新订阅 `ProgressHub.events` 恢复对话框，绝不再次解析 intent 重复提交**——否则同一分享会产生双份传输与两条 QUEUED 历史行。Intent 无可靠唯一 id 时，可用"提取出的 uri 列表拼接串"作为去重键，Activity 存续期内相同即视为同一 intent。
4. 调 `TransferEngine.submitShares(uris)`：
   - **PC 已连接且已配对** → 逐个 `SendTask` 立即发送（未配对则走下一分支）。
   - **未连接/未配对** → 把分享内容**复制**到 `context.getExternalFilesDir("outbox")/`（app 专属目录，免 URI 授权），历史行 `status=QUEUED`、`contentUri=缓存文件路径`；PC 稍后经 LIST/RECV 拉走，完成后删除缓存。
   - *为什么必须复制*：ACTION_SEND 授予的 URI 读权限随 Activity 销毁/进程结束失效，不可持久引用；缓存副本使 outbox 行永远可读（决策 D5）。
5. Activity **停留**显示进度对话框，订阅 `ProgressHub.events`（事件 lossy，见 A11；终态以 Room 状态为准），全部结束后才 `finish()`；用户中途划走导致授权失效时，`SendTask` 捕获 `SecurityException` → 行置 FAILED，协议层无感。
6. 大小未知（`OpenableColumns.SIZE < 0`）的流：先落 outbox 缓存取得确切 size 再走发送（协议要求 `size ≥ 0`）。

## 12. 并行开发守则

- 骨架先合入（第 3/5/6 节全部文件），三实现者基于骨架分叉。
- 跨模块只允许调第 4 节契约签名；契约如需变更，**先改本文档再改码**，避免三方 API 漂移。
- C 的 `RootScreen()` 是骨架 `MainActivity` 的编译依赖，骨架合入当天 C 必须先提交 C1 占位实现（空 Scaffold），否则骨架不编译。
- 各模块单测与本模块同包同主（第 4 节）；`gradle :app:assembleDebug` 全绿 + 各自模块测试绿 = 交付标准；B 额外跑 `connectedDebugAndroidTest`。
- 联调顺序建议：A+B 先打通 PC→手机 RECV；C 的 StatusPanel 完成后三方联调配对码全流程。

## 13. 关键决策记录

| # | 决策 | 理由与证据 |
|---|---|---|
| D1 | FGS 用 `specialUse`（非 `dataSync`） | Android 15 起 dataSync 有 6h/24h 上限且超时崩溃，与常驻监听冲突；specialUse 需 `FOREGROUND_SERVICE_SPECIAL_USE`+subtype 属性，无运行时前置。证据：behavior-changes-15、fg-service-types 原文（本会话抓取） |
| D2 | AGP 8.7.3 / Gradle 8.9 / Kotlin 2.0.21 / KSP 2.0.21-1.0.28 | AGP 8.7 最低要求 Gradle 8.9（与本机一致）、API 35 最低 AGP 8.6.0、AGP 需 JDK 17；四个版本号均经 Maven metadata 核实存在 |
| D3 | token 派生：HKDF-SHA256(CSPRNG 32B 种子， salt=配对码) → 32B token | 6 位码仅 20 位熵，直接派生可被在线爆破；种子保证 token 256 位熵，码只作一次性在线交换门槛。算法精确定义见 PROTOCOL.md §3.3 |
| D4 | 传输历史单表兼作续传状态源 | RUNNING/FAILED 行 + SAF 半成品 = 完整续传上下文；避免第二张状态表与两表一致性维护 |
| D5 | 离线分享复制进 outbox 缓存，不持久引用分享 URI | 分享 URI 授权随 Activity/进程销毁失效；缓存副本保证 QUEUED 行永远可读 |
| D6 | 共享资源防冲突：strings 按模块拆文件 + 键前缀强制 | Android 资源合并天然支持多 strings 文件，实现者永远不碰共享文件（第 2 节） |
