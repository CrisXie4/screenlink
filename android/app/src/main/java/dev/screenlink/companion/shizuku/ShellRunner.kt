package dev.screenlink.companion.shizuku

import android.os.ParcelFileDescriptor
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.async
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.delay
import kotlinx.coroutines.withContext
import kotlinx.coroutines.withTimeoutOrNull
import moe.shizuku.server.IRemoteProcess
import moe.shizuku.server.IShizukuService
import rikka.shizuku.Shizuku

/** 一次 shell 执行的结果。 */
data class ShellResult(
    val exitCode: Int,
    val stdout: String,
    val stderr: String,
)

/** shell 执行失败（超时 / 未捕获退出码标记 / binder 不可用等）。 */
class ShellException(
    message: String,
    val exitCode: Int? = null,
    val output: String = "",
) : Exception(message)

/**
 * C8：`IShizukuService.newProcess` 封装（DESIGN.md §10，API 签名已对 Shizuku api/aidl 13.1.5 javap 核实）：
 *  - `moe.shizuku.server.IShizukuService.Stub.asInterface(Shizuku.getBinder())`
 *    `.newProcess(String[], String[], String)` → `moe.shizuku.server.IRemoteProcess`（不是 ParcelFileDescriptor）；
 *  - `getInputStream()`=远端 stdout、`getOutputStream()`=远端 stdin（均为 ParcelFileDescriptor，
 *    经 `AutoCloseInputStream/OutputStream` 包装）、另有 `getErrorStream()`；
 *  - `alive()`/`waitFor()`/`exitValue()`/`destroy()`。
 *
 * 统一包装 `sh -c "<script>; echo __EXIT__=$?"` 从 stdout 尾部解析退出码——评审遗留规避：
 * 裸重定向命令（如 `cat > 文件`）不产生任何 stdout，若无尾标读 `__EXIT__` 会永久挂起。
 * [run]/[runWithStdin] 为挂起函数，内部以 IO 线程读流 + 轮询存活 + 总超时兜底，不会永久阻塞调用方。
 *
 * 安全边界：script 仅由本模块内部常量拼接（settings/wc/kill/cat/app_process），不接受外部输入；
 * 调用前置条件为 [ShizukuGate] 状态 Authorized（未授权时 requireService 快速抛出，UI 层已降级隐藏入口）。
 */
object ShellRunner {

    /** 退出码尾标标记（DESIGN.md §10 规定样式 `__EXIT__=$?`）。 */
    const val EXIT_MARKER = "__EXIT__"

    const val DEFAULT_TIMEOUT_MS = 15_000L

    private const val POLL_INTERVAL_MS = 100L
    private val EXIT_REGEX = Regex("$EXIT_MARKER=(\\d+)")

    /** 取 Shizuku binder 服务；binder 不可用时快速失败（IllegalStateException）。 */
    fun requireService(): IShizukuService {
        check(ShizukuGate.isBinderAvailable()) { "Shizuku binder 不可用（未安装或未运行）" }
        val binder = Shizuku.getBinder() ?: throw IllegalStateException("Shizuku binder 尚未就绪")
        return IShizukuService.Stub.asInterface(binder)
    }

    /** 以 shell 身份启动任意进程（argv 直传，不经二次 shell 转义）。 */
    fun newProcess(cmd: List<String>, env: List<String>? = null, dir: String? = null): IRemoteProcess =
        requireService().newProcess(cmd.toTypedArray(), env?.toTypedArray(), dir)

    /**
     * `sh -c "<script>; echo __EXIT__=$?"` 包装进程。`$?` 为 shell 展开的上一命令退出码，
     * 裸脚本（无输出）也能从尾标取回退出码。
     */
    fun newShellProcess(script: String, env: List<String>? = null): IRemoteProcess =
        newProcess(listOf("sh", "-c", "$script; echo $EXIT_MARKER=\$?"), env)

    /** 执行 script（不需要 stdin），返回 [ShellResult]。 */
    suspend fun run(script: String, timeoutMs: Long = DEFAULT_TIMEOUT_MS): ShellResult =
        runWithStdin(script, ByteArray(0), timeoutMs)

    /**
     * 执行 script 并向远端 stdin 写入 [stdin] 后关闭写端（供 `cat > 目标` 类命令接收数据）。
     * 读 stdout 与写 stdin 并发进行：cat 持续消费管道，几十 KB 级数据不会撑满缓冲死锁。
     */
    suspend fun runWithStdin(
        script: String,
        stdin: ByteArray,
        timeoutMs: Long = DEFAULT_TIMEOUT_MS,
    ): ShellResult = withContext(Dispatchers.IO) {
        val proc = newShellProcess(script)
        try {
            coroutineScope {
                val stdoutJob = async { proc.readAll(proc.inputStream) }
                val stderrJob = async { proc.readAll(proc.errorStream) }
                if (stdin.isNotEmpty()) {
                    ParcelFileDescriptor.AutoCloseOutputStream(proc.outputStream).use { it.write(stdin) }
                } else {
                    // 不写也要关闭写端：`cat` 类命令读到 EOF 才会退出
                    runCatching { proc.outputStream.close() }
                }
                val exitedInTime = withTimeoutOrNull(timeoutMs) {
                    while (proc.alive()) delay(POLL_INTERVAL_MS)
                    true
                }
                if (exitedInTime != true) {
                    runCatching { proc.destroy() }
                    throw ShellException("shell 命令超时（${timeoutMs}ms）：$script")
                }
                parse(stdoutJob.await(), stderrJob.await(), script)
            }
        } catch (t: Throwable) {
            runCatching { proc.destroy() }
            throw t
        }
    }

    private fun parse(stdout: String, stderr: String, script: String): ShellResult {
        val last = EXIT_REGEX.findAll(stdout).lastOrNull()
            ?: throw ShellException(
                "未捕获退出码尾标（$script）",
                output = (stdout + "\n" + stderr).take(2_000),
            )
        val exit = last.groupValues[1].toInt()
        val out = stdout.substring(0, last.range.first).trimEnd('\n', '\r')
        return ShellResult(exit, out, stderr)
    }

    /** 读到 EOF 为止（进程退出时远端关闭管道，自然 EOF；destroy() 也会触发 EOF 解除阻塞）。 */
    private suspend fun IRemoteProcess.readAll(pfd: ParcelFileDescriptor): String =
        withContext(Dispatchers.IO) {
            ParcelFileDescriptor.AutoCloseInputStream(pfd).use { input ->
                val buf = ByteArray(8 * 1024)
                val out = StringBuilder()
                while (true) {
                    val n = input.read(buf)
                    if (n < 0) break
                    out.append(String(buf, 0, n, Charsets.UTF_8))
                }
                out.toString()
            }
        }
}
