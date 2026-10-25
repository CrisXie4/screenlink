package dev.screenlink.companion.data

import android.content.Context
import kotlinx.coroutines.flow.Flow

/**
 * B4：DAO 薄封装（**无状态**，A、C 可各自实例化）；
 * 状态常量 STATUS_RUNNING/DONE/FAILED/CANCELLED/QUEUED 与 DIR_RECV/DIR_SEND 定义处。
 * 对外契约（DESIGN.md §4，签名不得变更）。
 */
class TransferRepository(private val dao: TransferDao, context: Context) {
    fun observeRecent(limit: Int = 200): Flow<List<TransferRecord>> = dao.observeRecent(limit)

    suspend fun enqueue(record: TransferRecord): Long = dao.insert(record)

    suspend fun updateProgress(id: Long, transferred: Long) = dao.updateProgress(id, transferred)

    suspend fun finish(id: Long, status: Int, md5: String?, finishedAt: Long, transferred: Long) =
        dao.finish(id, status, md5, finishedAt, transferred)

    /** direction=SEND, status=QUEUED（LIST 命令数据源） */
    suspend fun queuedForPc(): List<TransferRecord> = dao.queued()

    /** direction=RECV, status IN (RUNNING, FAILED)，倒序取 1 */
    suspend fun findResumable(fileName: String): TransferRecord? = dao.findResumable(fileName)

    /** 服务启动时把上次 RUNNING 收尸为 FAILED */
    suspend fun failAllRunning(now: Long) = dao.failAllRunning(now)

    suspend fun clear() = dao.clear()

    companion object {
        const val STATUS_RUNNING = 0
        const val STATUS_DONE = 1
        const val STATUS_FAILED = 2
        const val STATUS_CANCELLED = 3
        const val STATUS_QUEUED = 4
        const val DIR_RECV = 0
        const val DIR_SEND = 1
    }
}
