package dev.screenlink.companion.transfer

import android.content.Context
import android.net.Uri
import android.provider.OpenableColumns
import android.webkit.MimeTypeMap
import dev.screenlink.companion.core.Protocol
import dev.screenlink.companion.data.AppDatabase
import dev.screenlink.companion.data.TransferRecord
import dev.screenlink.companion.data.TransferRepository
import dev.screenlink.companion.net.AgentServer
import dev.screenlink.companion.net.AgentState
import dev.screenlink.companion.saf.SafStore
import dev.screenlink.companion.saf.SafWriter
import java.io.File
import java.io.IOException
import java.security.SecureRandom
import java.util.concurrent.atomic.AtomicLong
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Semaphore
import kotlinx.coroutines.sync.withPermit

/**
 * A8：xid 分配、并发上限（8）、SEND/RECV 任务调度、`submitShares` 门面（DESIGN.md §4/§11）。
 *
 * AgentServer 在连接上收到 SEND_REQ/DATA/CANCEL/… 时经本网关路由到具体任务；
 * 任务通过 [TransferLink] 回写帧。xid 全局（进程内）分配，连接断开由
 * [handleLinkClosed] 统一收尸。
 */
object TransferEngine {

    /** 对外契约（DESIGN.md §4，签名不得变更）。 */
    sealed interface SubmitResult {
        data class Queued(val recordIds: List<Long>) : SubmitResult          // PC 未连接：入 outbox
        data class Sent(val recordIds: List<Long>) : SubmitResult            // 已连接：立即发送
        data class NeedPairing(val recordIds: List<Long>) : SubmitResult     // 已监听但未配对：入 outbox + 显示配对码
    }

    /** 已认证 PC 连接的传输侧抽象（AgentServer.Link 实现）。 */
    interface TransferLink {
        val peerName: String
        val peerHost: String

        /** 连接是否仍存活且已认证。 */
        val isAlive: Boolean

        /** 发送 JSON 帧紧凑序列化后的 payload。 */
        suspend fun send(type: Int, jsonBody: Any?)

        /** 发送原始 payload（DATA 等）。 */
        suspend fun sendRaw(type: Int, payload: ByteArray)
    }

    /** 发送方向任务对等事件入口（AgentServer 收到 TRANSFER_DONE/PROGRESS/CANCEL 时调用）。 */
    interface OutboundTransfer {
        fun onSendAck(accept: Boolean, offset: Long, reason: String?)
        fun onPeerProgress(received: Long)
        fun onPeerDone(status: String, md5: String?)
        fun onPeerCancel(keep: Boolean, reason: String?)
        fun onLinkClosed()
    }

    /** 接收方向任务对等事件入口（AgentServer 收到 DATA/CANCEL 时调用）。 */
    interface InboundTransfer {
        fun onPeerData(chunk: ByteArray)
        fun onPeerCancel(keep: Boolean, reason: String?)
        fun onLinkClosed()
    }

    /** SEND_REQ 的 ACK 决策（reason 见 §5.3：NO_STORAGE/DENIED/BUSY/INVALID）。 */
    sealed interface AckDecision {
        data class Accepted(val offset: Long) : AckDecision
        data class Rejected(val reason: String) : AckDecision
    }

    /** 任务共享依赖（TransferEngine.init 时装配，传递给 SendTask/ReceiveTask）。 */
    class Deps internal constructor(
        val context: Context,
        val repo: TransferRepository,
        val safWriter: SafWriter,
        val outboxDir: File,
        val scope: CoroutineScope,
        val sendSemaphore: Semaphore,
        val nextXid: () -> Int,
    )

    @Volatile
    private var initialized = false

    private lateinit var deps: Deps
    private lateinit var safStore: SafStore

    private val xidCounter = AtomicLong(0x1000L + SecureRandom().nextInt(0x1000))

    /** xid → 活跃任务（SendTask | ReceiveTask）。 */
    private val active = java.util.concurrent.ConcurrentHashMap<Int, Any>()

    /** 正在发送中的 outbox 行 id → xid（防同一记录被并发 RECV）。 */
    private val sendingRecords = java.util.concurrent.ConcurrentHashMap<Long, Int>()

    fun nextXid(): Int {
        while (true) {
            val v = (xidCounter.incrementAndGet() and 0x0FFFFFFL).toInt()
            if (v != 0) return v // 协议规定 xid 非 0（§5）
        }
    }

    /**
     * 幂等（重复调用无副作用）。调用责任人：AgentService.onCreate 与
     * SendToPcActivity.onCreate（在 submitShares 之前），见 DESIGN.md §11。
     */
    fun init(appContext: Context) {
        if (initialized) return
        synchronized(this) {
            if (initialized) return
            val ctx = appContext.applicationContext
            val db = AppDatabase.get(ctx)
            val repo = TransferRepository(db.transferDao(), ctx)
            safStore = SafStore(ctx)
            val writer = SafWriter(safStore, ctx.contentResolver)
            val external = ctx.getExternalFilesDir("outbox")
            val outbox = external ?: File(ctx.filesDir, "outbox").also { it.mkdirs() }
            deps = Deps(
                context = ctx,
                repo = repo,
                safWriter = writer,
                outboxDir = outbox,
                scope = CoroutineScope(SupervisorJob() + Dispatchers.IO),
                sendSemaphore = Semaphore(Protocol.MAX_CONCURRENT_TRANSFERS),
                nextXid = ::nextXid,
            )
            initialized = true
        }
    }

    internal fun requireDeps(): Deps {
        check(initialized) { "TransferEngine.init(context) 必须先于使用调用（DESIGN.md §4 A8 契约）" }
        return deps
    }

    /** C 的分享页唯一入口；若 init 从未被调用须抛 IllegalStateException 快速失败。 */
    suspend fun submitShares(uris: List<Uri>): SubmitResult {
        check(initialized) { "TransferEngine.init(context) 必须先于 submitShares 调用（DESIGN.md §4 A8 契约）" }
        if (uris.isEmpty()) return SubmitResult.Queued(emptyList())

        val link = AgentServer.currentLink()
        val recordIds = ArrayList<Long>(uris.size)

        for (uri in uris) {
            try {
                val displayName = queryDisplayName(uri) ?: "share-${System.currentTimeMillis()}"
                val mime = queryMimeType(uri)
                var size = querySize(uri)

                if (link != null && size >= 0) {
                    // PC 已连接且大小已知：直接从分享源 Uri 发送（Activity 存续期内授权有效，§11.5）
                    val id = deps.repo.enqueue(
                        record(
                            displayName = displayName, sizeBytes = size, mime = mime,
                            status = TransferRepository.STATUS_RUNNING, contentUri = uri.toString(),
                            peerName = link.peerName, peerHost = link.peerHost,
                        ),
                    )
                    recordIds.add(id)
                    launchSend(link, id, displayName, size, mime, uri.toString(), fromOutbox = false)
                } else {
                    // 未连接 / 未配对 / 大小未知：复制进 outbox 缓存取得确切 size（决策 D5、§11.6）
                    val cached = copyToOutbox(uri, displayName, mime)
                    val status = if (link != null) TransferRepository.STATUS_RUNNING
                    else TransferRepository.STATUS_QUEUED
                    val id = deps.repo.enqueue(
                        record(
                            displayName = displayName, sizeBytes = cached.length(), mime = mime,
                            status = status, contentUri = cached.absolutePath,
                            peerName = link?.peerName ?: "", peerHost = link?.peerHost ?: "",
                        ),
                    )
                    recordIds.add(id)
                    if (link != null) {
                        launchSend(link, id, displayName, cached.length(), mime,
                            cached.absolutePath, fromOutbox = false)
                    }
                }
            } catch (e: Exception) {
                // 单个分享失败不拖垮整批：落一条 FAILED 行（SecurityException=授权失效等）
                val id = deps.repo.enqueue(
                    record(
                        displayName = "unknown", sizeBytes = 0, mime = null,
                        status = TransferRepository.STATUS_FAILED, contentUri = uri.toString(),
                        peerName = "", peerHost = "",
                    ),
                )
                recordIds.add(id)
            }
        }

        return when {
            link != null -> SubmitResult.Sent(recordIds)
            AgentServer.state.value is AgentState.AwaitingPairing -> SubmitResult.NeedPairing(recordIds)
            else -> SubmitResult.Queued(recordIds)
        }
    }

    // ------------------------------------------------------------------
    //  AgentServer 网关（已认证连接上的命令分发）
    // ------------------------------------------------------------------

    /** §5.3 SEND_REQ（PC → 手机）：校验、续传匹配、建档，返回 ACK 决策。 */
    suspend fun handleSendReq(
        link: TransferLink, xid: Int,
        name: String?, size: Long?, mime: String?, md5: String?,
    ): AckDecision {
        val d = requireDeps()
        if (active.size >= Protocol.MAX_CONCURRENT_TRANSFERS) return AckDecision.Rejected("BUSY")
        if (name.isNullOrEmpty() || !isValidFileName(name)) return AckDecision.Rejected("INVALID")
        if (size == null || size < 0) return AckDecision.Rejected("INVALID")
        if (!d.safWriter.hasValidTree()) return AckDecision.Rejected("DENIED")

        val task = ReceiveTask(d, link, xid, name, size, mime, md5?.lowercase())
        active[xid] = task
        val decision = try {
            task.negotiate()
        } catch (e: IOException) {
            // 建档/查询半成品时本地 IO 失败：ACK 无 IO_ERROR 语义，按空间类拒绝（§5.3）
            active.remove(xid)
            return AckDecision.Rejected("NO_STORAGE")
        }
        if (decision is AckDecision.Rejected) {
            active.remove(xid)
        } else {
            d.scope.launch { task.run() }
        }
        return decision
    }

    /** RECV_REQ 处理结果：started=false 时 reason 为回给 PC 的 SEND_ACK reason、ref 为回显。 */
    class RecvReqResult internal constructor(val started: Boolean, val reason: String?, val ref: String)

    /**
     * §5.2 RECV_REQ（PC 拉取 outbox）：启动 SendTask。
     * started=true 表示 SEND_REQ 将由任务发出；false 时 [RecvReqResult.reason] 为
     * `NOT_FOUND`（无此条目/缓存丢失）或 `BUSY`（该记录发送中），[RecvReqResult.ref] 为原样回显。
     */
    suspend fun handleRecvReq(link: TransferLink, id: String?, name: String?): RecvReqResult {
        val d = requireDeps()
        val ref = id ?: name ?: ""
        if (id == null && name == null) return RecvReqResult(false, "NOT_FOUND", ref)

        val record = d.repo.queuedForPc().firstOrNull { r ->
            (id != null && r.id.toString() == id) || (name != null && r.fileName == name)
        } ?: return RecvReqResult(false, "NOT_FOUND", ref)

        // 同一记录已在发送中 → 挂起哨兵回 BUSY（xid=0 + ref 回显，§5.2）
        if (sendingRecords.containsKey(record.id)) return RecvReqResult(false, "BUSY", ref)

        val file = record.contentUri?.let { File(it) }
        if (file == null || !file.isFile) return RecvReqResult(false, "NOT_FOUND", ref)

        sendingRecords[record.id] = 0
        launchOutboxSend(link, record, file)
        return RecvReqResult(true, null, ref)
    }

    fun handleData(xid: Int, chunk: ByteArray) {
        (active[xid] as? InboundTransfer)?.onPeerData(chunk)
        // 未知 xid 的 DATA 静默丢弃：CANCEL/收尾竞态下的迟到帧属正常（§6.4 隐式取消）
    }

    fun handleProgress(xid: Int, received: Long) {
        (active[xid] as? OutboundTransfer)?.onPeerProgress(received)
    }

    fun handleTransferDone(xid: Int, status: String, md5: String?) {
        (active[xid] as? OutboundTransfer)?.onPeerDone(status, md5)
    }

    fun handleCancel(xid: Int, keep: Boolean, reason: String?) {
        when (val task = active[xid]) {
            is OutboundTransfer -> task.onPeerCancel(keep, reason)
            is InboundTransfer -> task.onPeerCancel(keep, reason)
        }
    }

    /** 连接断开：该连接上所有活跃传输按隐式 CANCEL keep=true 处置（§6.4）。 */
    fun handleLinkClosed(link: TransferLink) {
        for ((xid, task) in active) {
            val belongs = when (task) {
                is SendTask -> task.link === link
                is ReceiveTask -> task.link === link
                else -> false
            }
            if (belongs) {
                when (task) {
                    is OutboundTransfer -> task.onLinkClosed()
                    is InboundTransfer -> task.onLinkClosed()
                }
                active.remove(xid)
            }
        }
    }

    internal fun unregister(xid: Int, recordId: Long?) {
        active.remove(xid)
        if (recordId != null) sendingRecords.remove(recordId)
    }

    internal fun register(xid: Int, task: Any) {
        active[xid] = task
    }

    // ------------------------------------------------------------------
    //  内部
    // ------------------------------------------------------------------

    private fun launchSend(
        link: TransferLink, recordId: Long, name: String, size: Long,
        mime: String?, source: String, fromOutbox: Boolean,
    ) {
        val record = TransferRecord(
            id = recordId, direction = TransferRepository.DIR_SEND,
            fileName = name, sizeBytes = size, mimeType = mime,
            peerName = link.peerName, peerHost = link.peerHost,
            status = TransferRepository.STATUS_RUNNING, transferredBytes = 0,
            md5 = null, startedAt = System.currentTimeMillis(), finishedAt = null,
            contentUri = source,
        )
        deps.scope.launch {
            deps.sendSemaphore.withPermit {
                try {
                    SendTask(deps, link, record, fromOutbox).run()
                } finally {
                    if (fromOutbox) sendingRecords.remove(recordId)
                }
            }
        }
    }

    private fun launchOutboxSend(link: TransferLink, record: TransferRecord, file: File) {
        deps.scope.launch {
            deps.sendSemaphore.withPermit {
                try {
                    SendTask(deps, link, record, fromOutbox = true).run()
                } finally {
                    sendingRecords.remove(record.id)
                }
            }
        }
    }

    private fun record(
        displayName: String, sizeBytes: Long, mime: String?, status: Int,
        contentUri: String, peerName: String, peerHost: String,
    ) = TransferRecord(
        direction = TransferRepository.DIR_SEND,
        fileName = displayName,
        sizeBytes = sizeBytes,
        mimeType = mime,
        peerName = peerName,
        peerHost = peerHost,
        status = status,
        transferredBytes = 0,
        md5 = null,
        startedAt = System.currentTimeMillis(),
        finishedAt = null,
        contentUri = contentUri,
    )

    /** 复制分享内容进 outbox（决策 D5：分享 URI 授权随 Activity 销毁失效，必须落本地副本）。 */
    private fun copyToOutbox(uri: Uri, displayName: String, mime: String?): File {
        deps.outboxDir.mkdirs()
        val safe = sanitizeFileName(displayName, mime)
        var target = File(deps.outboxDir, safe)
        var n = 1
        while (target.exists()) {
            val dot = safe.lastIndexOf('.')
            target = if (dot > 0) {
                File(deps.outboxDir, "${safe.substring(0, dot)} ($n)${safe.substring(dot)}")
            } else {
                File(deps.outboxDir, "$safe ($n)")
            }
            n++
        }
        deps.context.contentResolver.openInputStream(uri)?.use { input ->
            target.outputStream().use { output -> input.copyTo(output) }
        } ?: throw IOException("无法打开分享内容: $uri")
        return target
    }

    private fun sanitizeFileName(name: String, mime: String?): String {
        var cleaned = name.replace(Regex("[\\\\/:*?\"<>|]"), "_").trim().ifEmpty { "file" }
        if (!cleaned.contains('.')) {
            MimeTypeMap.getSingleton().getExtensionFromMimeType(mime)
                ?.let { ext -> cleaned = "$cleaned.$ext" }
        }
        return cleaned
    }

    private fun queryDisplayName(uri: Uri): String? = runCatching {
        deps.context.contentResolver.query(uri, null, null, null, null)?.use { cursor ->
            val col = cursor.getColumnIndex(OpenableColumns.DISPLAY_NAME)
            if (col >= 0 && cursor.moveToFirst()) cursor.getString(col) else null
        }
    }.getOrNull()

    private fun querySize(uri: Uri): Long = runCatching {
        deps.context.contentResolver.query(uri, null, null, null, null)?.use { cursor ->
            val col = cursor.getColumnIndex(OpenableColumns.SIZE)
            if (col >= 0 && cursor.moveToFirst() && !cursor.isNull(col)) cursor.getLong(col) else -1L
        } ?: -1L
    }.getOrDefault(-1L)

    private fun queryMimeType(uri: Uri): String? {
        if (uri == Uri.EMPTY) return null
        return runCatching { deps.context.contentResolver.getType(uri) }.getOrNull()
    }

    /** §5.3/§10：接收方必须净化 name（拒绝路径分隔符与 `..`）。 */
    private fun isValidFileName(name: String): Boolean =
        !name.contains('/') && !name.contains('\\') && name != ".." && !name.contains("..") &&
            name != "." && name.isNotBlank()
}
