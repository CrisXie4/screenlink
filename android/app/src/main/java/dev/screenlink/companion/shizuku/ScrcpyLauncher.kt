package dev.screenlink.companion.shizuku

import android.content.Context
import android.os.ParcelFileDescriptor
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import kotlinx.coroutines.withTimeoutOrNull
import moe.shizuku.server.IRemoteProcess
import java.util.concurrent.atomic.AtomicReference

/** scrcpy server 生命周期（SettingsPanel 卡片渲染）。 */
sealed interface ScrcpyState {
    /** 空闲（未启动，或 server 已退出恢复空闲——DESIGN.md §10.4）。 */
    data object Idle : ScrcpyState

    /** 正在推送 assets/scrcpy-server 到 /data/local/tmp。 */
    data object Pushing : ScrcpyState

    /** 已推送，app_process 启动中。 */
    data object Starting : ScrcpyState

    /** 宽限期内进程存活 → 视为就绪（tunnel 模式在 localabstract:scrcpy 上监听，等 PC 连接）。 */
    data class Running(val ip: String?, val sinceEpochMs: Long) : ScrcpyState

    /** 推送失败 / server 启动即退出。 */
    data class Failed(val reason: String) : ScrcpyState
}

/**
 * C10：经 Shizuku 以 shell 身份推送并启动 scrcpy server（DESIGN.md §10）。
 *
 *  1. 推送：`sh -c "cat > /data/local/tmp/scrcpy-server && chmod 644 …; echo __EXIT__=$?"`
 *     （ShellRunner C8 模式，规避裸重定向无 stdout 的挂起遗留），jar 字节经
 *     IRemoteProcess stdin（getOutputStream()）写入；远端大小一致时跳过重复推送。
 *  2. 启动（env 携带 CLASSPATH，与 scrcpy 官方 adb-shell 方式一致）：
 *     env=["CLASSPATH=/data/local/tmp/scrcpy-server"]，
 *     cmd=["app_process","/","com.genymobile.scrcpy.Server","3.1","log_level=info","stay_awake=true"]。
 *     server 无 sc_id 参数 → 监听 **localabstract:scrcpy**（scrcpy v3.1 源码
 *     DesktopConnection.getSocketName：scid==-1 时返回前缀 "scrcpy"，已对 v3.1 核实）。
 *  3. 就绪判定：server 静默监听期不产生 stdout，故以「宽限期内进程存活」为就绪；
 *     启动即退出 → Failed（附 stdout/stderr 摘要）。
 *  4. 退出：PC 断开后 server 自退出；后台读 stdout 至 EOF → 恢复 Idle。
 *
 * 资产：app/src/main/assets/scrcpy-server = scrcpy-server v3.1（90,640 字节，
 * sha256 958f0944a62f23b1f33a16e9eb14844c1a04b882ca175a738c16d23cb22b86c0）。
 */
object ScrcpyLauncher {

    const val SERVER_VERSION = "3.1"
    const val SERVER_ASSET = "scrcpy-server"
    const val REMOTE_PATH = "/data/local/tmp/scrcpy-server"
    const val LOCAL_SOCKET_NAME = "scrcpy"

    private const val READY_GRACE_MS = 4_000L
    private const val PUSH_TIMEOUT_MS = 20_000L
    private const val KILL_TIMEOUT_MS = 5_000L
    private const val OUTPUT_CAP = 64 * 1024

    // 独立 scope：server 监视任务的生命周期独立于任何 UI（进入设置页前仍需感知 EOF）
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.IO)

    private val _state = MutableStateFlow<ScrcpyState>(ScrcpyState.Idle)

    /** SettingsPanel 收集渲染。 */
    val state: StateFlow<ScrcpyState> = _state

    private val process = AtomicReference<IRemoteProcess?>(null)

    /**
     * 电脑侧配方（DESIGN.md §10.3 原文四步；pair 仅首次，配对端口来自 C9 的
     * _adb-tls-pairing 发现，连接端口来自 _adb-tls-connect 发现）。缺参时展示占位符。
     */
    fun pcRecipe(ip: String?, connectPort: Int?, pairingPort: Int?): String {
        val host = ip ?: "<手机IP>"
        val connect = connectPort?.let { "$host:$it" } ?: "<ip>:<无线调试端口>"
        val pair = pairingPort?.let { "$host:$it" } ?: "<ip>:<配对端口>"
        return buildString {
            appendLine("adb pair $pair   # 仅首次：输入系统配对对话框上的 6 位码")
            appendLine("adb connect $connect")
            appendLine("adb forward tcp:27183 localabstract:$LOCAL_SOCKET_NAME")
            append("scrcpy -s $connect --force-adb-forward")
        }
    }

    /** 推送（必要时）+ 启动 server；状态经 [state] 流转 Pushing→Starting→Running/Failed。 */
    suspend fun start(context: Context) {
        val app = context.applicationContext
        try {
            killPrevious()
            _state.value = ScrcpyState.Pushing
            pushServer(app)
            _state.value = ScrcpyState.Starting
            val proc = ShellRunner.newProcess(
                cmd = listOf(
                    "app_process",
                    "/",
                    "com.genymobile.scrcpy.Server",
                    SERVER_VERSION,
                    "log_level=info",
                    "stay_awake=true",
                ),
                env = listOf("CLASSPATH=$REMOTE_PATH"),
                dir = "/",
            )
            process.set(proc)
            val output = StringBuilder()
            val reader = scope.launch { drain(proc, output) }
            // 就绪判定：server 静默监听无 stdout，宽限期内存活即视为就绪
            val aliveAfterGrace = withTimeoutOrNull(READY_GRACE_MS) {
                while (proc.alive()) delay(150)
                false // 提前退出
            } ?: true // 超时仍存活
            if (!aliveAfterGrace) {
                runCatching { withTimeoutOrNull(2_000L) { reader.join() } }
                process.set(null)
                _state.value = ScrcpyState.Failed(
                    "server 启动即退出：" + synchronized(output) { output.toString() }
                        .ifBlank { "（无输出）" }.take(600),
                )
                return
            }
            _state.value = ScrcpyState.Running(WirelessDebugEnabler.localIpv4(), System.currentTimeMillis())
            scope.launch {
                // stdout EOF = server 退出（PC 断开自退出，DESIGN §10.4）→ 恢复空闲
                runCatching { reader.join() }
                if (process.compareAndSet(proc, null)) {
                    _state.value = ScrcpyState.Idle
                }
            }
        } catch (e: CancellationException) {
            throw e
        } catch (e: Exception) {
            process.set(null)
            _state.value = ScrcpyState.Failed(
                (e.message ?: e.javaClass.simpleName).take(600),
            )
        }
    }

    /** 手动停止 server（destroy → stdin/stdout 关闭 → 监视任务自然结束）。 */
    fun stop() {
        process.getAndSet(null)?.let { p -> runCatching { p.destroy() } }
        _state.value = ScrcpyState.Idle
    }

    /** 清理残留 server：占住 localabstract:scrcpy 会让新实例绑定失败。按 cmdline 匹配 kill（shell 权限）。 */
    private suspend fun killPrevious() {
        runCatching {
            ShellRunner.run(
                script = "for p in /proc/[0-9]*/cmdline; do grep -qa com.genymobile.scrcpy.Server \"\$p\" && kill -9 \"\${p%/cmdline}\"; done; true",
                timeoutMs = KILL_TIMEOUT_MS,
            )
        }
        delay(200)
    }

    private suspend fun pushServer(app: Context) {
        val bytes = withContext(Dispatchers.IO) {
            app.assets.open(SERVER_ASSET).use { it.readBytes() }
        }
        val sizeOut = ShellRunner.run("wc -c < $REMOTE_PATH 2>/dev/null || echo -1", timeoutMs = KILL_TIMEOUT_MS)
        if (sizeOut.exitCode == 0 && sizeOut.stdout.trim().toIntOrNull() == bytes.size) {
            return // 大小一致 → 跳过重复推送
        }
        val pushed = ShellRunner.runWithStdin(
            script = "cat > $REMOTE_PATH && chmod 644 $REMOTE_PATH",
            stdin = bytes,
            timeoutMs = PUSH_TIMEOUT_MS,
        )
        if (pushed.exitCode != 0) {
            throw ShellException(
                "推送 scrcpy-server 失败（exit=${pushed.exitCode}）：${pushed.stderr.ifBlank { pushed.stdout }.take(300)}",
            )
        }
        val verify = ShellRunner.run("wc -c < $REMOTE_PATH", timeoutMs = KILL_TIMEOUT_MS)
        if (verify.exitCode != 0 || verify.stdout.trim().toIntOrNull() != bytes.size) {
            throw ShellException("推送校验失败：远端 ${verify.stdout.trim()} 字节 ≠ 本地 ${bytes.size} 字节")
        }
    }

    /** 持续读 stdout/stderr 进诊断缓冲（供 Failed 展示），至 EOF 返回。 */
    private suspend fun drain(proc: IRemoteProcess, sink: StringBuilder) = withContext(Dispatchers.IO) {
        val stdoutTask = launch { drainStream(proc.inputStream, sink) }
        val stderrTask = launch { drainStream(proc.errorStream, sink) }
        stdoutTask.join()
        stderrTask.join()
    }

    private fun drainStream(pfd: ParcelFileDescriptor, sink: StringBuilder) {
        runCatching {
            ParcelFileDescriptor.AutoCloseInputStream(pfd).use { input ->
                val buf = ByteArray(8 * 1024)
                while (true) {
                    val n = input.read(buf)
                    if (n < 0) break
                    synchronized(sink) {
                        if (sink.length > OUTPUT_CAP) sink.setLength(0) // 长期运行防膨胀，粗截断
                        sink.append(String(buf, 0, n, Charsets.UTF_8))
                    }
                }
            }
        }
    }
}
