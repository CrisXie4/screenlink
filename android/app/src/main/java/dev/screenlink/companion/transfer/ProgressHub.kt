package dev.screenlink.companion.transfer

import kotlinx.coroutines.channels.BufferOverflow
import kotlinx.coroutines.flow.MutableSharedFlow

/**
 * A11：引擎→通知→UI 的进度总线。对外契约（DESIGN.md §4，签名不得变更）。
 * **必须显式 DROP_OLDEST**：默认 SUSPEND 会在无收集者时把 emit 永久挂起、卡死传输引擎。
 * 事件为 lossy（仅展示用）：订阅方不得假设逐帧必达；
 * 终态展示与流程推进以 Room 历史状态为最终事实来源（评审遗留：C6 的 finish 判定不得锚在本总线事件计数上）。
 */
sealed interface ProgressEvent {
    data class Started(val recordId: Long, val xid: Int, val direction: Int, val fileName: String, val total: Long) : ProgressEvent
    data class Progress(val recordId: Long, val transferred: Long, val total: Long) : ProgressEvent
    data class Finished(val recordId: Long, val ok: Boolean, val md5: String?) : ProgressEvent
}

object ProgressHub {
    val events: MutableSharedFlow<ProgressEvent> = MutableSharedFlow(
        replay = 0,
        extraBufferCapacity = 64,
        onBufferOverflow = BufferOverflow.DROP_OLDEST,
    )
}
