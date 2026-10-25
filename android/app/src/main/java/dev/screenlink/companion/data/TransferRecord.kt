package dev.screenlink.companion.data

import androidx.room.Entity
import androidx.room.Index
import androidx.room.PrimaryKey

/**
 * B1：Room 实体（DESIGN.md §7 逐字实现）。
 * 单表 transfer_history，兼作断点续传状态源（决策 D4）：
 * status=RUNNING/FAILED 的行 + SAF 半成品文档共同描述一次未完成接收。
 */
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
