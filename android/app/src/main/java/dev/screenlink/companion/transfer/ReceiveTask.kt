package dev.screenlink.companion.transfer

import android.net.Uri
import android.provider.OpenableColumns
import dev.screenlink.companion.core.MiniJson
import dev.screenlink.companion.core.Protocol
import dev.screenlink.companion.data.TransferRepository
import java.io.IOException
import java.io.OutputStream
import kotlinx.coroutines.channels.Channel

/**
 * A10：接收方向任务（PC → 手机），SAF 落盘（B 的 [dev.screenlink.companion.saf.SafWriter]）。
 *
 * - negotiate()：SEND_REQ 校验 → 续传匹配（Room 行 RUNNING/FAILED + SAF 同名半成品，决策 D4）
 *   → 建/复用文档与历史行 → SEND_ACK{accept,offset}；
 * - run()：offset>0 先对已有前缀 re-hash（§6.2.4），随后 "wa" 追加写入并滚动合并 MD5（§6.2.5）；
 * - 收满即结算（§6.3/§6.4）：MD5_MISMATCH 删半成品；IO_ERROR 保留；OK 改名（SAF 天然无 .part）；
 * - PROGRESS 节奏：每 4 MiB 或 500ms 至少一帧（§5.4）；
 * - 对端 CANCEL keep=true / 连接断开（隐式 CANCEL keep=true）保留半成品供续传。
 */
class ReceiveTask(
    private val deps: TransferEngine.Deps,
    internal val link: TransferEngine.TransferLink,
    private val xid: Int,
    private val name: String,
    private val size: Long,
    private val mime: String?,
    private val expectedMd5: String?, // 小写 32 hex 或 null
) : TransferEngine.InboundTransfer {

    private val dataChannel = Channel<ByteArray>(Channel.UNLIMITED)

    @Volatile private var cancelRequested = false
    @Volatile private var cancelKeep = true
    @Volatile private var linkAlive = true

    private var rowId: Long = -1L
    private var docUri: Uri? = null
    private var fileName = name
    private var offsetAcked = 0L

    // ---- TransferEngine.InboundTransfer（AgentServer 读循环调用，非挂起） ----

    override fun onPeerData(chunk: ByteArray) {
        if (!cancelRequested && linkAlive) dataChannel.trySend(chunk)
        // 通道无限缓冲：背压由 TCP 窗口承担（§5.4）；超量数据来自对端违约，v1 接受
    }

    override fun onPeerCancel(keep: Boolean, reason: String?) {
        cancelKeep = keep
        cancelRequested = true
        dataChannel.close()
    }

    override fun onLinkClosed() {
        linkAlive = false
        dataChannel.close()
    }

    // ---- 协商（engine.handleSendReq 内同步调用；成功后任务经 scope.launch(run()) 接管） ----

    suspend fun negotiate(): TransferEngine.AckDecision {
        // 引擎已做过 name/size/树授权校验；此处只做续传匹配与建档
        val row = deps.repo.findResumable(name)
        val doc = deps.safWriter.findByName(name)

        val offset: Long = if (row != null && doc != null && doc.second in 1..size) {
            // 命中半成品：复用文档与历史行（半成品 size ≤ 请求 size → offset = 半成品 size，§6.2.2）
            docUri = doc.first
            rowId = row.id
            doc.second
        } else {
            // 未命中：建档（提供方可能自动改名 → 回查真名写历史行，DESIGN.md §8.4）
            val uri = deps.safWriter.createUnique(name, mime ?: "application/octet-stream")
            docUri = uri
            fileName = queryDisplayName(uri) ?: name
            rowId = deps.repo.enqueue(
                dev.screenlink.companion.data.TransferRecord(
                    direction = TransferRepository.DIR_RECV,
                    fileName = fileName,
                    sizeBytes = size,
                    mimeType = mime,
                    peerName = link.peerName,
                    peerHost = link.peerHost,
                    status = TransferRepository.STATUS_RUNNING,
                    transferredBytes = 0,
                    md5 = null,
                    startedAt = System.currentTimeMillis(),
                    finishedAt = null,
                    contentUri = uri.toString(),
                ),
            )
            0L
        }
        offsetAcked = offset

        link.send(
            Protocol.T_SEND_ACK,
            MiniJson.obj("xid" to xid.toLong(), "accept" to true, "offset" to offset),
        )
        ProgressHub.events.emit(
            ProgressEvent.Started(rowId, xid, TransferRepository.DIR_RECV, fileName, size),
        )
        return TransferEngine.AckDecision.Accepted(offset)
    }

    // ---- 接收主流程 ----

    suspend fun run() {
        val md = Digests.Incremental()
        var out: OutputStream? = null
        var received = offsetAcked
        try {
            if (offsetAcked > 0) {
                // 写入任何续传字节之前，对已有前缀完成一次 re-hash（§6.2.4）
                rehashPrefix(md)
            }
            val uri = checkNotNull(docUri)
            out = if (offsetAcked > 0) deps.safWriter.openAppend(uri)
            else deps.safWriter.openOverwrite(uri)

            var bytesSinceFrame = 0L
            var lastFrameAt = System.currentTimeMillis()
            var lastFlushAt = 0L

            while (received < size) {
                val chunk = dataChannel.receiveCatching().getOrNull() ?: break // cancel / link closed
                if (cancelRequested) break
                out.write(chunk)
                md.update(chunk)
                received += chunk.size
                bytesSinceFrame += chunk.size

                val now = System.currentTimeMillis()
                if (bytesSinceFrame >= PROGRESS_FRAME_BYTES || now - lastFrameAt >= PROGRESS_FRAME_MS) {
                    // §5.4：每收满 4 MiB 或每 500ms 至少一帧 PROGRESS（允许丢帧）
                    runCatching {
                        link.send(
                            Protocol.T_PROGRESS,
                            MiniJson.obj("xid" to xid.toLong(), "received" to received),
                        )
                    }
                    bytesSinceFrame = 0
                    lastFrameAt = now
                }
                if (now - lastFlushAt >= PROGRESS_FLUSH_MS) {
                    lastFlushAt = now
                    deps.repo.updateProgress(rowId, received)
                    ProgressHub.events.emit(ProgressEvent.Progress(rowId, received, size))
                }
            }

            when {
                cancelRequested -> finalizeCancel(received)
                !linkAlive -> finalizeInterrupted(received)
                else -> finalizeComplete(received, md.hex())
            }
        } catch (e: IOException) {
            finalizeIoError(received)
        } finally {
            runCatching { out?.close() }
            dataChannel.close()
            TransferEngine.unregister(xid, rowId)
        }
    }

    /** 收满 size 后结算（§6.3/§6.4）。 */
    private suspend fun finalizeComplete(received: Long, md5Hex: String) {
        val now = System.currentTimeMillis()
        val mismatch = expectedMd5 != null && !expectedMd5.equals(md5Hex, ignoreCase = true)
        if (mismatch) {
            deps.safWriter.delete(checkNotNull(docUri)) // 半成品删除：重试只能全量
            runCatching {
                link.send(
                    Protocol.T_TRANSFER_DONE,
                    MiniJson.obj("xid" to xid.toLong(), "status" to "MD5_MISMATCH", "md5" to md5Hex),
                )
            }
            deps.repo.finish(rowId, TransferRepository.STATUS_FAILED, md5Hex, now, received)
            ProgressHub.events.emit(ProgressEvent.Finished(rowId, false, md5Hex))
        } else {
            runCatching {
                link.send(
                    Protocol.T_TRANSFER_DONE,
                    MiniJson.obj("xid" to xid.toLong(), "status" to "OK", "md5" to md5Hex),
                )
            }
            deps.repo.finish(rowId, TransferRepository.STATUS_DONE, md5Hex, now, received)
            ProgressHub.events.emit(ProgressEvent.Finished(rowId, true, md5Hex))
        }
    }

    private suspend fun finalizeCancel(received: Long) {
        if (!cancelKeep || (received == 0L && offsetAcked == 0L)) {
            // keep=false 删除（§6.4）；一字节未收的空文档无续传价值，也一并清理
            deps.safWriter.delete(checkNotNull(docUri))
        }
        deps.repo.finish(rowId, TransferRepository.STATUS_CANCELLED, null, System.currentTimeMillis(), received)
        ProgressHub.events.emit(ProgressEvent.Finished(rowId, false, null))
    }

    private suspend fun finalizeInterrupted(received: Long) {
        // 连接意外断开 = 隐式 CANCEL keep=true（§6.4）：保留半成品与 FAILED 行供 offset 续传
        if (received == 0L && offsetAcked == 0L) deps.safWriter.delete(checkNotNull(docUri))
        deps.repo.finish(rowId, TransferRepository.STATUS_FAILED, null, System.currentTimeMillis(), received)
        ProgressHub.events.emit(ProgressEvent.Finished(rowId, false, null))
    }

    private suspend fun finalizeIoError(received: Long) {
        // §6.3：本地写失败 → status IO_ERROR、保留半成品，后续可 offset 续传
        runCatching {
            link.send(
                Protocol.T_TRANSFER_DONE,
                MiniJson.obj("xid" to xid.toLong(), "status" to "IO_ERROR", "md5" to null),
            )
        }
        deps.repo.finish(rowId, TransferRepository.STATUS_FAILED, null, System.currentTimeMillis(), received)
        ProgressHub.events.emit(ProgressEvent.Finished(rowId, false, null))
    }

    private fun rehashPrefix(md: Digests.Incremental) {
        val uri = checkNotNull(docUri)
        deps.context.contentResolver.openInputStream(uri)?.use { input ->
            var remaining = offsetAcked
            val buf = ByteArray(64 * 1024)
            while (remaining > 0) {
                val want = minOf(buf.size.toLong(), remaining).toInt()
                val n = input.read(buf, 0, want)
                if (n < 0) throw IOException("半成品比已 ACK 的 offset 短（被外部修改）")
                md.update(buf, 0, n)
                remaining -= n
            }
        } ?: throw IOException("无法打开半成品文档")
    }

    private fun queryDisplayName(uri: Uri): String? = runCatching {
        deps.context.contentResolver.query(uri, null, null, null, null)?.use { cursor ->
            val col = cursor.getColumnIndex(OpenableColumns.DISPLAY_NAME)
            if (col >= 0 && cursor.moveToFirst()) cursor.getString(col) else null
        }
    }.getOrNull()

    private companion object {
        const val PROGRESS_FRAME_BYTES = 4L * 1024 * 1024
        const val PROGRESS_FRAME_MS = 500L
        const val PROGRESS_FLUSH_MS = 250L
    }
}
