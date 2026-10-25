package dev.screenlink.companion.shizuku

import android.content.Context
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import java.io.File

/**
 * Shell 模式 agent 管理器（DESIGN.md §10 扩展）：
 * 把 APK 自身作为 classpath，用 Shizuku `app_process` 以 shell uid 启动
 * [dev.screenlink.companion.agent.AgentMain]——MIUI/HyperOS 不冻结 shell 进程，
 * 息屏/切后台均不影响传输链路（App 进程被冻结只影响本地 UI 与 Room，不影响 agent）。
 *
 * 生命周期：
 *  - install：把 base.apk 复制到 /data/local/tmp/sl-agent.apk（17MB 走 stdin 管道）；
 *  - start：`app_process` 后台拉起（重定向 + `&`，ShellRunner 的 __EXIT__ 立即返回）；
 *  - isAlive：`pgrep -f AgentMain`；
 *  - stop：`pkill -f AgentMain`。
 *
 * token 摘要以参数传给 AgentMain（App 不出明文 token，PC 持明文）。
 * 保存目录由调用方决定（默认 /sdcard/Download/ScreenLink，shell 有全量存储权限，无 SAF 依赖）。
 */
class AgentShell(private val context: Context) {

    private val remoteApk = "/data/local/tmp/sl-agent.apk"

    /** 复制 APK 到 /data/local/tmp 并 chmod；幂等。首次约 17MB，走 stdin 管道。 */
    suspend fun install(): Unit = withContext(Dispatchers.IO) {
        val bytes = File(context.applicationInfo.sourceDir).readBytes()
        ShellRunner.runWithStdin(
            "cat > $remoteApk && chmod 644 $remoteApk",
            bytes,
            timeoutMs = 60_000L,
        )
    }

    @Volatile private var keepProc: moe.shizuku.server.IRemoteProcess? = null

    /** 启动 shell agent（先 pkill 旧实例）。返回启动是否成功（以进程出现为准）。 */
    suspend fun start(
        tokenSha256Hex: String,
        storageDir: String = "/sdcard/Download/ScreenLink",
        port: Int = 27188,
    ): Boolean = withContext(Dispatchers.IO) {
        runCatching { ShellRunner.run("pkill -f AgentMain", timeoutMs = 5_000L) }
        // setsid 换新会话 + 保引用防 GC destroy：两重保险防后台进程被连带杀掉
        val script = "setsid sh -c 'CLASSPATH=$remoteApk app_process / " +
            "dev.screenlink.companion.agent.AgentMain $port \"$storageDir\" $tokenSha256Hex " +
            ">/data/local/tmp/sl-agent.log 2>&1' &"
        val proc = ShellRunner.newShellProcess(script)
        keepProc = proc // 持有引用：IRemoteProcess 被 GC 终结时会 destroy 远端进程组
        repeat(20) {
            if (isAlive()) return@withContext true
            kotlinx.coroutines.delay(250)
        }
        false
    }

    suspend fun stop(): Boolean = withContext(Dispatchers.IO) {
        runCatching { keepProc?.destroy() }
        keepProc = null
        ShellRunner.run("pkill -f AgentMain", timeoutMs = 5_000L)
        repeat(10) {
            if (!isAlive()) return@withContext true
            kotlinx.coroutines.delay(200)
        }
        !isAlive()
    }

    /** shell agent 是否存活（pgrep -f AgentMain）。 */
    suspend fun isAlive(): Boolean = withContext(Dispatchers.IO) {
        runCatching {
            // [A] 技巧：防止 pgrep -f 匹配到检查命令自身的 shell 进程（假阳性）
            ShellRunner.run("pgrep -f \"[A]gentMain\"", timeoutMs = 5_000L).exitCode == 0
        }.getOrDefault(false)
    }

    /** agent 日志尾部（排障）。 */
    suspend fun logTail(): String = withContext(Dispatchers.IO) {
        runCatching {
            ShellRunner.run("tail -20 /data/local/tmp/sl-agent.log", timeoutMs = 5_000L).stdout
        }.getOrDefault("")
    }
}

/** agent 运行模式（prefs：screenlink_prefs / shell_agent_on）。shell 模式下 App 内置 AgentServer 不启动，27188 归 shell agent。 */
object AgentModePrefs {
    private const val PREFS_NAME = "screenlink_prefs"
    private const val KEY = "shell_agent_on"

    fun isShellMode(context: Context): Boolean =
        context.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE).getBoolean(KEY, false)

    fun set(context: Context, on: Boolean) {
        context.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE).edit().putBoolean(KEY, on).apply()
    }
}
