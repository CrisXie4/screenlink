package dev.screenlink.companion.service

import android.app.NotificationManager
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import androidx.core.app.ServiceCompat
import androidx.core.content.ContextCompat
import dev.screenlink.companion.R
import dev.screenlink.companion.core.Protocol
import dev.screenlink.companion.data.AppDatabase
import dev.screenlink.companion.data.TransferRepository
import dev.screenlink.companion.net.AgentServer
import dev.screenlink.companion.net.AgentState
import dev.screenlink.companion.transfer.ProgressEvent
import dev.screenlink.companion.transfer.ProgressHub
import dev.screenlink.companion.transfer.TransferEngine
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.launchIn
import kotlinx.coroutines.flow.onEach
import kotlinx.coroutines.launch

/**
 * A6：前台服务薄壳（DESIGN.md §9）。
 *
 * - onCreate：TransferEngine.init → 收尸（failAllRunning）→ AgentServer.start →
 *   **ServiceCompat.startForeground(this, id, notif, FOREGROUND_SERVICE_TYPE_SPECIAL_USE)**。
 *   禁止直接调三参 startForeground（API 29+ 才有，minSdk 26 真机 NoSuchMethodError）；
 * - 订阅 AgentServer.state 刷新常驻通知；订阅 ProgressHub.events 以 250ms 节流刷新进度通知
 *   （事件 lossy 仅展示，DROP_OLDEST 丢帧不重发；终态以 Room 历史为事实来源）；
 * - 服务被系统回收后由用户在 UI 重新启动（不实现开机自启，START_NOT_STICKY）。
 */
class AgentService : Service() {

    private val serviceScope = CoroutineScope(SupervisorJob() + Dispatchers.Default)

    /** recordId → [transferred, total, direction]（进度聚合）。 */
    private val activeTransfers = HashMap<Long, LongArray>()

    /** recordId → 文件名（进度通知标题）。 */
    private val namesById = HashMap<Long, String>()

    @Volatile private var lastProgressFlush = 0L

    override fun onCreate() {
        super.onCreate()
        TransferEngine.init(applicationContext)
        AgentNotifications.ensureChannels(this)

        // 上次进程被杀时 RUNNING 的接收行收尸为 FAILED（半成品 SAF 文档保留供续传，§9）
        serviceScope.launch {
            val repo = TransferRepository(
                AppDatabase.get(applicationContext).transferDao(), applicationContext,
            )
            repo.failAllRunning(System.currentTimeMillis())
        }

        ServiceCompat.startForeground(
            this, AgentNotifications.NOTIF_AGENT_ID,
            AgentNotifications.agentNotification(this, statusText(AgentServer.state.value)),
            ServiceInfo.FOREGROUND_SERVICE_TYPE_SPECIAL_USE,
        )

        // Shizuku shell 模式：27188 归 shell 进程的 AgentMain，内置 server 不启动
        if (!dev.screenlink.companion.shizuku.AgentModePrefs.isShellMode(this)) {
            AgentServer.start(this)
        } else {
            android.util.Log.i("AgentService", "shell agent 模式：内置 AgentServer 跳过")
        }

        AgentServer.state
            .onEach { state ->
                val nm = getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager
                nm.notify(
                    AgentNotifications.NOTIF_AGENT_ID,
                    AgentNotifications.agentNotification(this, statusText(state)),
                )
            }
            .launchIn(serviceScope)

        ProgressHub.events
            .onEach { event -> onProgressEvent(event) }
            .launchIn(serviceScope)
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        if (intent?.action == AgentNotifications.ACTION_STOP) stopSelf()
        return START_NOT_STICKY
    }

    override fun onBind(intent: Intent?): android.os.IBinder? = null

    override fun onDestroy() {
        serviceScope.cancel()
        AgentServer.stop(this)
        ServiceCompat.stopForeground(this, ServiceCompat.STOP_FOREGROUND_REMOVE)
        super.onDestroy()
    }

    // ---- 通知渲染 ----

    private fun statusText(state: AgentState): String = when (state) {
        is AgentState.Idle -> getString(R.string.sl_svc_notif_listening_no_ip, Protocol.PORT)
        is AgentState.Listening ->
            if (state.ip != null) getString(R.string.sl_svc_notif_listening_text, state.ip, state.port)
            else getString(R.string.sl_svc_notif_listening_no_ip, state.port)
        is AgentState.AwaitingPairing ->
            getString(
                R.string.sl_svc_notif_pairing_text, state.code,
                formatRemaining(state.expiresAtEpochMs - System.currentTimeMillis()),
                state.ip ?: "",
            )
        is AgentState.Paired -> getString(R.string.sl_svc_notif_paired_text, state.peerName)
        is AgentState.Failed -> getString(R.string.sl_svc_notif_listening_no_ip, Protocol.PORT)
    }

    private fun formatRemaining(msLeft: Long): String {
        val s = (msLeft / 1000).coerceAtLeast(0)
        return if (s >= 60) "${s / 60}m${s % 60}s" else "${s}s"
    }

    private fun onProgressEvent(event: ProgressEvent) {
        val nm = getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager
        when (event) {
            is ProgressEvent.Started -> {
                activeTransfers[event.recordId] =
                    longArrayOf(0, event.total, event.direction.toLong())
                namesById[event.recordId] = event.fileName
                flushProgress(nm, force = true)
            }
            is ProgressEvent.Progress -> {
                activeTransfers[event.recordId]?.let {
                    it[0] = event.transferred
                    it[1] = event.total
                }
                flushProgress(nm)
            }
            is ProgressEvent.Finished -> {
                val name = namesById[event.recordId]
                activeTransfers.remove(event.recordId)
                namesById.remove(event.recordId)
                flushProgress(nm, force = true)
                val resultId = AgentNotifications.resultNotificationId(event.recordId)
                nm.notify(
                    resultId,
                    AgentNotifications.transferResult(this, event.ok, name ?: ""),
                )
                // 「完成/失败发一次即清除」（§9）：3.5s 后撤下结果通知
                serviceScope.launch {
                    delay(3_500)
                    runCatching { nm.cancel(resultId) }
                }
            }
        }
    }

    /** 250ms 节流（§9）；force 用于 Started/Finished 立即刷新。 */
    private fun flushProgress(nm: NotificationManager, force: Boolean = false) {
        val now = System.currentTimeMillis()
        if (!force && now - lastProgressFlush < PROGRESS_THROTTLE_MS) return
        lastProgressFlush = now

        if (activeTransfers.isEmpty()) {
            nm.cancel(AgentNotifications.NOTIF_TRANSFER_ID)
            return
        }
        var sumTransferred = 0L
        var sumTotal = 0L
        var firstId = -1L
        var firstValue: LongArray? = null
        for ((id, v) in activeTransfers) {
            if (firstValue == null) {
                firstId = id
                firstValue = v
            }
            sumTransferred += v[0]
            sumTotal += v[1]
        }
        val direction = checkNotNull(firstValue)[2].toInt()
        val title = if (activeTransfers.size > 1) {
            getString(R.string.sl_svc_transfer_multi, activeTransfers.size)
        } else {
            val template = if (direction == TransferRepository.DIR_RECV)
                R.string.sl_svc_transfer_single_recv
            else R.string.sl_svc_transfer_single_send
            getString(template, namesById[firstId] ?: "")
        }
        nm.notify(
            AgentNotifications.NOTIF_TRANSFER_ID,
            AgentNotifications.transferProgress(this, title, sumTransferred, sumTotal),
        )
    }

    companion object {
        private const val PROGRESS_THROTTLE_MS = 250L

        fun start(context: Context) {
            ContextCompat.startForegroundService(
                context, Intent(context, AgentService::class.java),
            )
        }

        fun stop(context: Context) {
            // 直接 stopService：onDestroy 反向清理（AgentServer.stop）。服务未运行时为 no-op。
            context.stopService(Intent(context, AgentService::class.java))
        }
    }
}
