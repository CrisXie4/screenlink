package dev.screenlink.companion.shizuku

import android.content.pm.PackageManager
import android.os.Handler
import android.os.Looper
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlin.concurrent.Volatile
import rikka.shizuku.Shizuku

/**
 * C7：Shizuku 可用性 / 授权状态（DESIGN.md §10）。
 *
 * - 可用性：`Shizuku.pingBinder()`（manifest 已声明 `queries` 包可见性与 ShizukuProvider）；
 *   false → 卡片显示「未安装/未运行 Shizuku」，所有 Shizuku 功能降级为不可用（运行时可选模块）。
 * - 授权：`Shizuku.requestPermission(code)` + `OnRequestPermissionResultListener`；
 *   已授权判定 `Shizuku.checkSelfPermission() == PackageManager.PERMISSION_GRANTED`。
 *
 * 状态机：Unavailable（binder 不在）→ Unauthorized（binder 在，未授权）→ Authorized。
 * SettingsPanel 通过 [attach]/[detach] 管理监听器生命周期（DisposableEffect）。
 * 所有 Shizuku API 调用都经 runCatching 包裹——binder 死亡、管理器版本过旧等均降级为 Unavailable，不抛 UI 崩溃。
 */
sealed interface ShizukuStatus {
    /** 未安装 / 未运行 / binder 已死亡 */
    data object Unavailable : ShizukuStatus

    /** binder 可用但未授权 */
    data object Unauthorized : ShizukuStatus

    /** 已授权（shell 身份可用） */
    data object Authorized : ShizukuStatus
}

object ShizukuGate {

    /** requestPermission 的回调码；本应用内 Shizuku 授权只有这一个入口，固定即可。 */
    const val PERMISSION_REQUEST_CODE = 27188

    private val mainHandler = Handler(Looper.getMainLooper())

    private val _status = MutableStateFlow<ShizukuStatus>(ShizukuStatus.Unavailable)

    /** SettingsPanel 收集渲染 */
    val status: StateFlow<ShizukuStatus> = _status

    @Volatile
    private var attached = false

    private val binderReceivedListener = Shizuku.OnBinderReceivedListener { refresh() }
    private val binderDeadListener = Shizuku.OnBinderDeadListener {
        _status.value = ShizukuStatus.Unavailable
    }
    private val permissionResultListener =
        Shizuku.OnRequestPermissionResultListener { _, _ -> refresh() }

    /** Shizuku binder 是否可用（未安装 / 未运行 / 已死亡返回 false）。 */
    fun isBinderAvailable(): Boolean =
        runCatching { Shizuku.pingBinder() }.getOrDefault(false)

    /** 注册 Shizuku 事件监听（幂等）；sticky 监听器在已连接时立即回调一次。 */
    fun attach() {
        synchronized(this) {
            if (attached) return
            attached = true
            runCatching {
                Shizuku.addBinderReceivedListenerSticky(binderReceivedListener, mainHandler)
                Shizuku.addBinderDeadListener(binderDeadListener, mainHandler)
                Shizuku.addRequestPermissionResultListener(permissionResultListener, mainHandler)
            }.onFailure {
                attached = false
                return
            }
        }
        refresh()
    }

    /** 注销监听（幂等）。 */
    fun detach() {
        synchronized(this) {
            if (!attached) return
            attached = false
        }
        runCatching { Shizuku.removeBinderReceivedListener(binderReceivedListener) }
        runCatching { Shizuku.removeBinderDeadListener(binderDeadListener) }
        runCatching { Shizuku.removeRequestPermissionResultListener(permissionResultListener) }
    }

    /** 立即重算状态（「重新检测」按钮、监听回调共用）。 */
    fun refresh() {
        _status.value = compute()
    }

    /**
     * 触发 Shizuku 授权对话框；结果经 OnRequestPermissionResultListener → [refresh] 反映到 [status]。
     * binder 不可用时为无操作（调用方 UI 已降级）。
     */
    fun requestPermission() {
        if (!isBinderAvailable()) return
        runCatching { Shizuku.requestPermission(PERMISSION_REQUEST_CODE) }
    }

    private fun compute(): ShizukuStatus {
        if (!isBinderAvailable()) return ShizukuStatus.Unavailable
        val granted = runCatching { Shizuku.checkSelfPermission() }
            .getOrNull() == PackageManager.PERMISSION_GRANTED
        return if (granted) ShizukuStatus.Authorized else ShizukuStatus.Unauthorized
    }
}
