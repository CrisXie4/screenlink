package dev.screenlink.companion.service

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import dev.screenlink.companion.MainActivity
import dev.screenlink.companion.R

/**
 * A7：通知渠道与通知构造（DESIGN.md §9）。
 *
 * - `sl_agent`（IMPORTANCE_LOW）：常驻监听通知（IP:端口 / 配对码 / 已配对文本），
 *   附「停止」Action（PendingIntent → AgentService ACTION_STOP）；
 * - `sl_transfer`（IMPORTANCE_LOW）：传输进度条（AgentService 内 250ms 节流刷新），
 *   完成/失败发一次结果通知后自动清除。
 */
object AgentNotifications {
    const val CHANNEL_AGENT = "sl_agent"
    const val CHANNEL_TRANSFER = "sl_transfer"

    const val NOTIF_AGENT_ID = 1001

    /** 进度通知固定槽位；结果通知用 [resultNotificationId] 每记录一槽（互不覆盖）。 */
    const val NOTIF_TRANSFER_ID = 1002
    fun resultNotificationId(recordId: Long): Int =
        (NOTIF_TRANSFER_ID + 1 + (recordId % 100_000L)).toInt()

    const val ACTION_STOP = "dev.screenlink.companion.action.AGENT_STOP"

    fun ensureChannels(context: Context) {
        val manager = context.getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager
        manager.createNotificationChannel(
            NotificationChannel(
                CHANNEL_AGENT,
                context.getString(R.string.sl_svc_channel_agent_name),
                NotificationManager.IMPORTANCE_LOW,
            ).apply { description = context.getString(R.string.sl_svc_channel_agent_desc) },
        )
        manager.createNotificationChannel(
            NotificationChannel(
                CHANNEL_TRANSFER,
                context.getString(R.string.sl_svc_channel_transfer_name),
                NotificationManager.IMPORTANCE_LOW,
            ).apply { description = context.getString(R.string.sl_svc_channel_transfer_desc) },
        )
    }

    /** 常驻监听通知：statusText 由 AgentService 按 AgentState 渲染。 */
    fun agentNotification(context: Context, statusText: String): Notification {
        val contentIntent = PendingIntent.getActivity(
            context, 0,
            Intent(context, MainActivity::class.java),
            PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
        )
        val stopIntent = PendingIntent.getService(
            context, 0,
            Intent(context, AgentService::class.java).setAction(ACTION_STOP),
            PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
        )
        return Notification.Builder(context, CHANNEL_AGENT)
            .setSmallIcon(R.drawable.ic_stat_screenlink)
            .setContentTitle(context.getString(R.string.sl_svc_notif_listening_title))
            .setContentText(statusText)
            .setContentIntent(contentIntent)
            .setOngoing(true)
            .setOnlyAlertOnce(true)
            .setStyle(Notification.BigTextStyle().bigText(statusText))
            .addAction(
                Notification.Action.Builder(
                    null,
                    context.getString(R.string.sl_svc_action_stop),
                    stopIntent,
                ).build(),
            )
            .build()
    }

    /** 传输进度通知（AgentService 250ms 节流后调用）。total ≤ 0 时显示不确定进度。 */
    fun transferProgress(
        context: Context, title: String,
        transferred: Long, total: Long,
    ): Notification = Notification.Builder(context, CHANNEL_TRANSFER)
        .setSmallIcon(R.drawable.ic_stat_screenlink)
        .setContentTitle(context.getString(R.string.sl_svc_transfer_title))
        .setContentText(title)
        .setOngoing(true)
        .setOnlyAlertOnce(true)
        .setProgress(100, percent(transferred, total), total <= 0)
        .build()

    /** 完成/失败结果通知：发一次即清除（AgentService 延时 cancel）。 */
    fun transferResult(context: Context, ok: Boolean, fileName: String): Notification =
        Notification.Builder(context, CHANNEL_TRANSFER)
            .setSmallIcon(R.drawable.ic_stat_screenlink)
            .setContentTitle(
                context.getString(
                    if (ok) R.string.sl_svc_transfer_done else R.string.sl_svc_transfer_failed,
                ),
            )
            .setContentText(fileName)
            .setAutoCancel(true)
            .build()

    private fun percent(transferred: Long, total: Long): Int =
        if (total <= 0) 0 else (transferred * 100 / total).toInt().coerceIn(0, 100)
}
