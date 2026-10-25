package dev.screenlink.companion.transfer

import android.net.Uri
import android.os.ParcelFileDescriptor
import dev.screenlink.companion.core.Protocol
import dev.screenlink.companion.data.TransferRecord
import dev.screenlink.companion.data.TransferRepository
import java.io.File
import java.io.FileInputStream
import java.io.IOException
import java.io.InputStream
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.withTimeoutOrNull

/**
 * A9：发送方向任务（手机 → PC）。
 *
 * - SEND_REQ{xid,name,size,mime,md5=null} → 等 SEND_ACK → 按 offset 续传（seek/skip 前缀）；
 * - 源为流且 offset>0 不可 seek 时按 PROTOCOL.md §6.2.3：CANCEL{keep:false} 清半成品 → 新 xid 全量重开
 *   （SEND_REQ 无 offset 字段，禁止同名重开——接收方会再 ACK 同一 offset 形成死循环）；
 * - DATA 帧 ≤ 1 MiB（§5.5）；TRANSFER_DONE 按表处置（§6.4）；
 * - 分享 URI 授权失效（SecurityException）→ 行置 FAILED（DESIGN.md §11.5）；
 * - outbox 拉取路径（fromOutbox=true）：成功后删除缓存副本；ACK 拒绝时保留 QUEUED 行供再次 RECV。
 *
 * 事件经 [inbox] 通道从 AgentServer 读循环线程投递（单写者语义：每事件独立投递）。
 */
class SendTask(
    private val deps: TransferEngine.Deps,
    internal val link: TransferEngine.TransferLink,
    private val record: TransferRecord,
    private val fromOutbox: Boolean,
) : TransferEngine.OutboundTransfer {

    private sealed interface Evt {
        data class Ack(val accept: Boolean, val offset: Long, val reason: String?) : Evt
        data class Done(val status: String, val md5: String?) : Evt
        data class Cancel(val keep: Boolean, val reason: String?) : Evt
        data object LinkClosed : Evt
    }

    private val inbox = Channel<Evt>(Channel.UNLIMITED)

    private var xid: Int = 0

    /** 已送达（含续传前缀）字节；失败收尾时写历史行用。 */
    @Volatile
    private var transferredNow = 0L

    // ---- TransferEngine.OutboundTransfer（AgentServer 读循环调用，非挂起） ----

    override fun onSendAck(accept: Boolean, offset: Long, reason: String?) {
        inbox.trySend(Evt.Ack(accept, offset, reason))
    }

    override fun onPeerProgress(received: Long) {
        // 信息性（§5.4），仅可用于对端速率展示；v1 不据此回调
    }

    override fun onPeerDone(status: String, md5: String?) {
        inbox.trySend(Evt.Done(status, md5))
    }

    override fun onPeerCancel(keep: Boolean, reason: String?) {
        inbox.trySend(Evt.Cancel(keep, reason))
    }

    override fun onLinkClosed() {
        inbox.trySend(Evt.LinkClosed)
    }

    // ---- 主流程 ----

    suspend fun run() {
        xid = deps.nextXid()
        TransferEngine.register(xid, this)
        var ok = false
        ProgressHub.events.emit(
            ProgressEvent.Started(record.id, xid, TransferRepository.DIR_SEND, record.fileName, record.sizeBytes),
        )
        try {
            ok = execute()
        } finally {
            TransferEngine.unregister(xid, if (fromOutbox) record.id else null)
            ProgressHub.events.emit(ProgressEvent.Finished(record.id, ok, null))
        }
    }

    private suspend fun execute(): Boolean {
        var attempts = 0
        while (attempts < 2) {
            attempts++
            try {
                link.send(
                    Protocol.T_SEND_REQ,
                    dev.screenlink.companion.core.MiniJson.obj(
                        "xid" to xid.toLong(), "name" to record.fileName,
                        "size" to record.sizeBytes, "mime" to record.mimeType,
                        "md5" to null, // 发送方不预计算（§6.3：接收方仍算全文件 MD5）
                    ),
                )
                when (val e = awaitAck()) {
                    null -> return failRow("ack timeout")                       // 30s 无 ACK
                    is Evt.Done -> return onPeerDoneSync(e.status, e.md5)
                    is Evt.Cancel -> return onPeerCancelSync(e.keep)
                    is Evt.LinkClosed -> return failRow("link closed")
                    is Evt.Ack -> {
                        if (!e.accept) {
                            return if (fromOutbox) {
                                false // 保留 QUEUED 行与缓存，PC 可再次 RECV
                            } else {
                                failRow("rejected: ${e.reason}")
                            }
                        }
                        val outcome = streamFrom(e.offset) ?: continue // 不可 seek：已 CANCEL，换新 xid 重开
                        return outcome
                    }
                }
            } catch (e: IOException) {
                return failRow("io: ${e.message}")
            } catch (e: SecurityException) {
                return failRow("share uri permission lost") // DESIGN.md §11.5
            }
        }
        return failRow("unseekable retry exhausted")
    }

    /** 等 SEND_ACK（超时 30s）。期间到达的终态事件直接上抛。 */
    private suspend fun awaitAck(): Evt? = withTimeoutOrNull(ACK_TIMEOUT_MS) {
        var e: Evt = inbox.receive()
        while (e !is Evt.Ack && e !is Evt.Done && e !is Evt.Cancel && e !is Evt.LinkClosed) {
            e = inbox.receive()
        }
        e
    }

    /**
     * 从 offset 起流式发送。返回 null = 源不可 seek 且 offset>0，已按 §6.2.3
     * 发 CANCEL keep=false，调用方应换新 xid 重开全量。
     */
    private suspend fun streamFrom(offset: Long): Boolean? {
        val source = openSource() ?: return failRow("source unreadable")
        val seekable = source.second
        var input: InputStream? = null
        try {
            input = source.first
            if (offset > 0) {
                val skipped = try {
                    skipFully(input, offset)
                    true
                } catch (e: IOException) {
                    false
                }
                if (!skipped) {
                    // §6.2.3：CANCEL keep=false 清接收方半成品 → 新 xid 全量重开
                    runCatching {
                        link.send(
                            Protocol.T_CANCEL,
                            dev.screenlink.companion.core.MiniJson.obj(
                                "xid" to xid.toLong(), "keep" to false, "reason" to "unseekable",
                            ),
                        )
                    }
                    input.close()
                    TransferEngine.unregister(xid, null)
                    xid = deps.nextXid()
                    TransferEngine.register(xid, this)
                    return null
                }
            }

            var transferred = offset
            var lastRepoFlush = 0L
            val buf = ByteArray(Protocol.MAX_DATA_CHUNK)
            while (transferred < record.sizeBytes) {
                // 发送间隙吸收对端终态事件（PC 中途取消/DONE）
                while (true) {
                    val e = inbox.tryReceive().getOrNull() ?: break
                    when (e) {
                        is Evt.Done -> return onPeerDoneSync(e.status, e.md5)
                        is Evt.Cancel -> return onPeerCancelSync(e.keep)
                        is Evt.LinkClosed -> return failRow("link closed")
                        is Evt.Ack -> {}
                    }
                }
                val want = minOf(buf.size.toLong(), record.sizeBytes - transferred).toInt()
                val n = input.read(buf, 0, want)
                if (n < 0) throw IOException("source shorter than declared size")
                if (n == 0) continue
                val payload = ByteArray(4 + n)
                payload[0] = ((xid ushr 24) and 0xFF).toByte()
                payload[1] = ((xid ushr 16) and 0xFF).toByte()
                payload[2] = ((xid ushr 8) and 0xFF).toByte()
                payload[3] = (xid and 0xFF).toByte()
                System.arraycopy(buf, 0, payload, 4, n)
                link.sendRaw(Protocol.T_DATA, payload)
                transferred += n
                transferredNow = transferred

                val now = System.currentTimeMillis()
                if (now - lastRepoFlush >= PROGRESS_FLUSH_MS) {
                    lastRepoFlush = now
                    deps.repo.updateProgress(record.id, transferred)
                    ProgressHub.events.emit(
                        ProgressEvent.Progress(record.id, transferred, record.sizeBytes),
                    )
                }
            }

            return awaitDone(offset, transferred)
        } finally {
            runCatching { input?.close() }
        }
    }

    /** 全部字节发出后等 TRANSFER_DONE / CANCEL（对端写盘+MD5 耗时，放宽到 300s）。 */
    private suspend fun awaitDone(offset: Long, transferred: Long): Boolean =
        withTimeoutOrNull(DONE_TIMEOUT_MS) {
            while (true) {
                when (val e = inbox.receive()) {
                    is Evt.Done -> return@withTimeoutOrNull onPeerDoneSync(e.status, e.md5)
                    is Evt.Cancel -> return@withTimeoutOrNull onPeerCancelSync(e.keep)
                    is Evt.LinkClosed -> return@withTimeoutOrNull failRow("link closed")
                    is Evt.Ack -> {}
                }
            }
            @Suppress("UNREACHABLE_CODE")
            false
        } ?: failRow("done timeout")

    // ---- 行处置（§6.4 / 设计 D4：Room 行为终态事实来源） ----

    private suspend fun onPeerDoneSync(status: String, md5: String?): Boolean {
        val now = System.currentTimeMillis()
        return when (status) {
            "OK" -> {
                deps.repo.finish(record.id, TransferRepository.STATUS_DONE, md5, now, record.sizeBytes)
                if (fromOutbox) deleteCache()
                true
            }
            "MD5_MISMATCH" -> {
                // 接收方已删其半成品；本端源未动，重试只能全量（§6.3）
                deps.repo.finish(record.id, TransferRepository.STATUS_FAILED, md5, now, record.sizeBytes)
                false
            }
            else -> { // IO_ERROR：接收方保留了半成品可续传；本端缓存保留与否不影响 QUEUED 失效
                deps.repo.finish(record.id, TransferRepository.STATUS_FAILED, null, now, record.sizeBytes)
                false
            }
        }
    }

    private suspend fun onPeerCancelSync(keep: Boolean): Boolean {
        deps.repo.finish(
            record.id, TransferRepository.STATUS_CANCELLED, null,
            System.currentTimeMillis(), record.sizeBytes,
        )
        if (fromOutbox && !keep) deleteCache()
        return false
    }

    private suspend fun failRow(reason: String): Boolean {
        deps.repo.finish(
            record.id, TransferRepository.STATUS_FAILED, null,
            System.currentTimeMillis(), transferredNow,
        )
        if (fromOutbox) deleteCache() // FAILED 行不再出现在 outbox 队列，缓存即垃圾
        return false
    }

    private fun deleteCache() {
        if (!fromOutbox) return
        record.contentUri?.let { path ->
            val f = File(path)
            if (f.isFile) runCatching { f.delete() }
        }
    }

    /** @return (流, 是否可 seek)。outbox 缓存走文件；分享 Uri 优先 FileDescriptor（seekable）。 */
    private fun openSource(): Pair<InputStream, Boolean>? {
        val path = record.contentUri ?: return null
        val f = File(path)
        if (f.isFile) return Pair(FileInputStream(f), true)
        val uri = Uri.parse(path)
        return try {
            val pfd = deps.context.contentResolver.openFileDescriptor(uri, "r")
            if (pfd != null) Pair(ParcelFileDescriptor.AutoCloseInputStream(pfd), true) else null
        } catch (e: IOException) {
            try {
                deps.context.contentResolver.openInputStream(uri)?.let { Pair(it, false) }
            } catch (e2: Exception) {
                null
            }
        }
    }

    private fun skipFully(input: InputStream, count: Long) {
        var remaining = count
        while (remaining > 0) {
            val skipped = input.skip(remaining)
            if (skipped > 0) {
                remaining -= skipped
            } else {
                // skip 返回 0：流不支持原地跳过，读一个字节推进
                if (input.read() < 0) throw IOException("stream shorter than resume offset")
                remaining -= 1
            }
        }
    }

    private companion object {
        const val ACK_TIMEOUT_MS = 30_000L
        const val DONE_TIMEOUT_MS = 300_000L
        const val PROGRESS_FLUSH_MS = 250L
    }
}
