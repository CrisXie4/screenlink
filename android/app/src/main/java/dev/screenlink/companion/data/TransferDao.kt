package dev.screenlink.companion.data

import androidx.room.Dao
import androidx.room.Insert
import androidx.room.Query
import kotlinx.coroutines.flow.Flow

/**
 * B2：DAO（DESIGN.md §7 逐字实现）。
 */
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
