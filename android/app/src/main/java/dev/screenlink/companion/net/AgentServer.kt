package dev.screenlink.companion.net

import android.content.Context
import android.net.nsd.NsdManager
import android.net.nsd.NsdServiceInfo
import android.os.Build
import android.util.Log
import dev.screenlink.companion.core.Frame
import dev.screenlink.companion.core.FrameCodec
import dev.screenlink.companion.core.MiniJson
import dev.screenlink.companion.core.MiniJson.optBoolean
import dev.screenlink.companion.core.MiniJson.optLong
import dev.screenlink.companion.core.MiniJson.optString
import dev.screenlink.companion.core.Protocol
import dev.screenlink.companion.transfer.Digests
import dev.screenlink.companion.transfer.TransferEngine
import java.io.BufferedInputStream
import java.io.BufferedOutputStream
import java.io.EOFException
import java.io.IOException
import java.net.Inet4Address
import java.net.Inet6Address
import java.net.InetSocketAddress
import java.net.NetworkInterface
import java.net.ServerSocket
import java.net.Socket
import java.net.SocketTimeoutException
import java.security.MessageDigest
import java.util.Collections
import java.util.concurrent.ConcurrentLinkedQueue
import java.util.concurrent.atomic.AtomicInteger
import java.util.concurrent.atomic.AtomicReference
import kotlinx.coroutines.CoroutineName
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext
import kotlinx.coroutines.withTimeoutOrNull

/**
 * A5：TCP 监听 27188 的接入层（PROTOCOL.md §1-§7）。
 *
 * - accept 循环：私网/链路本地过滤 → 同 IP 冷却检查 → 未认证并发 ≤4 → 每连接一个协程；
 * - 连接状态机：互发 HELLO（5s）→ AUTH（HELLO 后 10s；进入配对流程后挂起）→
 *   NEED_PAIR/PAIR（码 TTL 300s、5 次失败作废）→ 已认证命令分发（LIST/RECV/SEND/DATA/…）；
 * - 单已认证会话：第 2 个认证请求 → ERR BUSY 并断开（§1）；
 * - 限速：AUTH+PAIR 校验失败按源 IP 合并计数，10min 窗口 5 次 → 冷却 60s（§3.1）；
 * - 保活：空闲 >30s 由本端发 PING，75s 无任何入帧 close（§7）；
 * - mDNS：注册 `_screenlink._tcp`，TXT pv/nm/id/pa（§2；TXT 值非 ASCII 时跳过该键）。
 *
 * 对外契约（DESIGN.md §4，签名不得变更）：[AgentState]、[state]、[start]/[stop]。
 */
sealed interface AgentState {
    data object Idle : AgentState
    data class Listening(val ip: String?, val port: Int) : AgentState
    data class AwaitingPairing(val ip: String?, val code: String, val expiresAtEpochMs: Long) : AgentState
    data class Paired(val ip: String?, val peerName: String) : AgentState
    data class Failed(val reason: String) : AgentState
}

object AgentServer {
    private const val TAG = "AgentServer"

    /** 未认证连接允许的帧（§3.1）；其余一律 ERR AUTH 后 close。 */
    private val UNAUTH_ALLOWED = setOf(
        Protocol.T_HELLO, Protocol.T_AUTH, Protocol.T_PAIR,
        Protocol.T_PING, Protocol.T_PONG, Protocol.T_BYE, Protocol.T_ERR,
    )

    private val _state = MutableStateFlow<AgentState>(AgentState.Idle)

    /** C 的 StatusPanel 收集渲染 */
    val state: StateFlow<AgentState> = _state

    @Volatile private var running = false
    private var scope: CoroutineScope? = null
    private var serverSocket: ServerSocket? = null
    private var store: PairingStore? = null
    private var nsdManager: NsdManager? = null
    private val nsdListener = object : NsdManager.RegistrationListener {
        override fun onServiceRegistered(serviceInfo: NsdServiceInfo) {
            Log.i(TAG, "mDNS 已注册: ${serviceInfo.serviceName}")
        }

        override fun onRegistrationFailed(serviceInfo: NsdServiceInfo, errorCode: Int) {
            Log.w(TAG, "mDNS 注册失败 code=$errorCode（TCP 仍可按 IP 直连）")
        }

        override fun onServiceUnregistered(serviceInfo: NsdServiceInfo) {
            Log.i(TAG, "mDNS 已注销")
        }

        override fun onUnregistrationFailed(serviceInfo: NsdServiceInfo, errorCode: Int) {
            Log.w(TAG, "mDNS 注销失败 code=$errorCode")
        }
    }

    private val links = ConcurrentLinkedQueue<Link>()
    private val authedLink = AtomicReference<Link?>(null)
    private val unauthCount = AtomicInteger(0)

    /** 同 IP AUTH+PAIR 失败时间戳（滑动窗口）与冷却截止（内存计数，重启清零，§3.1）。 */
    private val ipFailures = HashMap<String, MutableList<Long>>()
    private val cooldownUntil = HashMap<String, Long>()

    /** 由 AgentService 调；内部拉起 mDNS 注册（NsdManager，PROTOCOL.md §2） */
    fun start(context: Context) {
        if (running) return
        val app = context.applicationContext
        TransferEngine.init(app)
        val s = PairingStore(app)

        val socket = ServerSocket()
        try {
            socket.reuseAddress = true
            socket.bind(InetSocketAddress(Protocol.PORT))
        } catch (e: Exception) {
            _state.value = AgentState.Failed("无法监听端口 ${Protocol.PORT}: ${e.message}")
            Log.e(TAG, "bind 失败", e)
            return
        }

        store = s
        serverSocket = socket
        running = true
        val sc = CoroutineScope(SupervisorJob() + Dispatchers.IO + CoroutineName("AgentServer"))
        scope = sc
        _state.value = AgentState.Listening(localIpv4(), Protocol.PORT)
        sc.launch { acceptLoop(socket) }
        registerNsd(app, s)
    }

    fun stop(context: Context) {
        if (!running) return
        running = false
        unregisterNsd()
        PairingCoordinator.invalidate()

        // 对端礼貌收尾：BYE{reason:shutdown}（§5.6，发送前缓冲已逐帧 flush）
        val live = links.toList()
        if (live.isNotEmpty()) {
            val closer = CoroutineScope(SupervisorJob() + Dispatchers.IO)
            closer.launch {
                for (l in live) {
                    withTimeoutOrNull(1_000) {
                        runCatching { l.send(Protocol.T_BYE, MiniJson.obj("reason" to "shutdown")) }
                    }
                    l.closeQuietly()
                }
            }
        }

        runCatching { serverSocket?.close() }
        serverSocket = null
        scope?.cancel()
        scope = null
        authedLink.set(null)
        links.clear()
        unauthCount.set(0)
        _state.value = AgentState.Idle
    }

    /** 已认证连接（TransferEngine.submitShares 判断能否立即发送）。未认证/无连接返回 null。 */
    internal fun currentLink(): TransferEngine.TransferLink? =
        authedLink.get()?.takeIf { it.authed && it.isAlive }

    // ------------------------------------------------------------------
    //  接入
    // ------------------------------------------------------------------

    private suspend fun acceptLoop(socket: ServerSocket) = withContext(Dispatchers.IO) {
        while (running) {
            val client = try {
                socket.accept()
            } catch (e: Exception) {
                if (running) Log.w(TAG, "accept 异常：$e")
                break
            }
            val addr = client.inetAddress
            val ip = addr.hostAddress ?: ""
            if (!isAllowedPeer(addr)) {
                client.closeQuietly(); continue
            }
            val now = System.currentTimeMillis()
            synchronized(cooldownUntil) {
                if (now < (cooldownUntil[ip] ?: 0L)) { // 冷却期间新连接直接 close（§3.1）
                    client.closeQuietly(); null
                } else {
                    true
                }
            } ?: continue
            if (unauthCount.get() >= Protocol.MAX_UNAUTH_CONNECTIONS) {
                client.closeQuietly(); continue
            }
            unauthCount.incrementAndGet()
            scope?.launch { handleConnection(client, ip) }
        }
    }

    /** §1 接入过滤：10/8、172.16/12、192.168/16、169.254/16、fc00::/7、fe80::/10。 */
    private fun isAllowedPeer(addr: java.net.InetAddress): Boolean = when (addr) {
        is Inet4Address -> {
            val b = addr.address
            b[0] == 10.toByte() ||
                (b[0] == 172.toByte() && (b[1].toInt() and 0xF0) == 0x10) ||
                (b[0] == 192.toByte() && b[1] == 168.toByte()) ||
                (b[0] == 169.toByte() && b[1] == 254.toByte())
        }
        is Inet6Address ->
            addr.isLinkLocalAddress || (addr.address[0].toInt() and 0xFE) == 0xFC
        else -> false
    }

    private fun localIpv4(): String? = runCatching {
        val candidates = mutableListOf<String>()
        for (nif in Collections.list(NetworkInterface.getNetworkInterfaces())) {
            for (ia in nif.inetAddresses) {
                if (ia is Inet4Address && !ia.isLoopbackAddress && isAllowedPeer(ia)) {
                    if (ia.isSiteLocalAddress) return@runCatching ia.hostAddress // 优先站点内网
                    ia.hostAddress?.let(candidates::add)
                }
            }
        }
        candidates.firstOrNull()
    }.getOrNull()

    // ------------------------------------------------------------------
    //  连接状态机
    // ------------------------------------------------------------------

    private suspend fun handleConnection(socket: Socket, ip: String) {
        val link = Link(socket, ip)
        links.add(link)
        var authFailures = 0        // 单连接 AUTH+PAIR 合并计数，5 次 close（§7）
        var helloDone = false
        var pairingPending = false  // 进入配对流程后 AUTH 10s 计时挂起（§3.2/§7）
        var pairingSession: PairingCoordinator.Session? = null
        var ownsPairingState = false // 本连接正在展示配对码（状态机归属判断用）
        var pingSentAt = 0L
        var lastInbound = System.currentTimeMillis()
        val helloDeadline = lastInbound + Protocol.HELLO_TIMEOUT_MS
        var authDeadline = Long.MAX_VALUE

        try {
            socket.soTimeout = SOCKET_POLL_MS
            link.send(
                Protocol.T_HELLO,
                MiniJson.obj(
                    "role" to "phone", "name" to Build.MODEL,
                    "app" to Protocol.APP_ID, "pv" to Protocol.VERSION.toLong(),
                ),
            )

            loop@ while (running && !socket.isClosed) {
                val frame = try {
                    FrameCodec.read(link.input)
                } catch (e: SocketTimeoutException) {
                    null
                } catch (e: EOFException) {
                    break
                } catch (e: FrameCodec.PayloadTooLarge) {
                    fatal(link, "PAYLOAD_TOO_LARGE", e.message ?: "payload too large"); break
                } catch (e: FrameCodec.ProtocolError) {
                    fatal(link, "PROTOCOL", e.message ?: "protocol error"); break
                } catch (e: IOException) {
                    break // 连接重置等传输层错误：按隐式 CANCEL 收尾
                }

                val now = System.currentTimeMillis()
                if (frame != null) {
                    lastInbound = now
                    pingSentAt = 0
                }

                // ---- 定时器（§7）：每轮至多 5s 醒一次 ----
                if (!helloDone && now >= helloDeadline) {
                    fatal(link, "TIMEOUT", "HELLO not received in ${Protocol.HELLO_TIMEOUT_MS}ms"); break
                }
                if (helloDone && !link.authed && !pairingPending && now >= authDeadline) {
                    fatal(link, "TIMEOUT", "AUTH not received in ${Protocol.AUTH_TIMEOUT_MS}ms"); break
                }
                if (frame == null) {
                    val idle = now - lastInbound
                    if (idle >= Protocol.IDLE_CLOSE_MS) break // 75s 无任何入帧 → close
                    if (idle >= Protocol.PING_AFTER_IDLE_MS && now - pingSentAt >= Protocol.PING_AFTER_IDLE_MS) {
                        // 空闲方（可能是等待人工输入配对码的本端）发 PING；对端 PONG 即续命
                        pingSentAt = now
                        runCatching { link.send(Protocol.T_PING, MiniJson.obj()) }
                    }
                    continue
                }

                when (frame.type) {
                    Protocol.T_HELLO -> {
                        if (helloDone) { fatal(link, "PROTOCOL", "duplicate HELLO"); break@loop }
                        val obj = runCatching { frame.jsonObject() }.getOrNull()
                        if (obj == null) { fatal(link, "PROTOCOL", "bad HELLO json"); break@loop }
                        if (obj.optString("role") != "pc" || obj.optLong("pv") != Protocol.VERSION.toLong()) {
                            fatal(link, "PROTOCOL", "role/pv mismatch"); break@loop
                        }
                        link.peerName = obj.optString("name") ?: "PC"
                        helloDone = true
                        authDeadline = now + Protocol.AUTH_TIMEOUT_MS
                    }

                    Protocol.T_AUTH -> {
                        if (link.authed) { fatal(link, "PROTOCOL", "already authenticated"); break@loop }
                        if (!helloDone) { fatal(link, "PROTOCOL", "AUTH before HELLO"); break@loop }
                        val r = handleAuth(link, ip, frame, authFailures)
                        authFailures = r.failures
                        if (r.pairingStarted) {
                            // NEED_PAIR 已发出：AUTH 10s 计时挂起，配对流程由码 TTL/5 次约束（§3.2）
                            ownsPairingState = true
                            pairingPending = true
                            pairingSession = PairingCoordinator.activeSession(now)
                        }
                        if (authFailures >= MAX_CONN_AUTH_FAILURES) {
                            fatal(link, "AUTH", "too many auth failures"); break@loop
                        }
                    }

                    Protocol.T_PAIR -> {
                        if (link.authed) { fatal(link, "PROTOCOL", "already authenticated"); break@loop }
                        val session = pairingSession
                        if (!pairingPending || session == null) {
                            fatal(link, "PROTOCOL", "no pairing session"); break@loop
                        }
                        val code = runCatching { frame.jsonObject().optString("code") }
                            .getOrElse { null }
                        val result = PairingCoordinator.check(session, code ?: "", now)
                        authFailures++
                        when (result) {
                            is PairingCoordinator.CheckResult.Accepted -> {
                                val s = result.session
                                if (!markAuthed(link)) {
                                    fatal(link, "BUSY", "another PC session is active")
                                    break@loop
                                }
                                store?.saveToken(s.tokenId, s.tokenSha256)
                                link.send(
                                    Protocol.T_PAIR_OK,
                                    MiniJson.obj(
                                        "token" to s.token, "token_id" to s.tokenId,
                                        "name" to Build.MODEL,
                                    ),
                                )
                                pairingSession = null
                                pairingPending = false
                                ownsPairingState = false
                            }
                            is PairingCoordinator.CheckResult.Rejected -> {
                                link.send(
                                    Protocol.T_PAIR_FAIL,
                                    MiniJson.obj("attempts_left" to result.attemptsLeft.toLong()),
                                )
                                recordIpFailure(ip)
                                if (result.attemptsLeft == 0) {
                                    // 码作废/过期：PC 需重发 AUTH 触发新码（§3.2）
                                    PairingCoordinator.invalidate()
                                    pairingSession = null
                                    if (ownsPairingState) resetToListening()
                                } else {
                                    pairingSession = PairingCoordinator.activeSession(now)
                                }
                                if (authFailures >= MAX_CONN_AUTH_FAILURES) {
                                    fatal(link, "AUTH", "too many auth failures"); break@loop
                                }
                            }
                        }
                    }

                    Protocol.T_PING -> link.send(Protocol.T_PONG, MiniJson.obj())
                    Protocol.T_PONG -> Unit
                    Protocol.T_BYE -> {
                        // 对端须回 BYE，双方随后关闭
                        runCatching { link.send(Protocol.T_BYE, MiniJson.obj("reason" to "bye")) }
                        break@loop
                    }
                    Protocol.T_ERR -> break@loop // 对端报告严重错误，其对端即本端 → close

                    Protocol.T_LIST_REQ, Protocol.T_RECV_REQ, Protocol.T_SEND_REQ,
                    Protocol.T_DATA, Protocol.T_PROGRESS, Protocol.T_TRANSFER_DONE,
                    Protocol.T_CANCEL,
                    -> {
                        if (!link.authed) { fatal(link, "AUTH", "not authenticated"); break@loop }
                        dispatchCommand(link, frame)
                    }

                    else -> {
                        // 未知帧类型且 version==1（codec 已校验）：读掉忽略（§4 向前兼容）
                        if (!link.authed && frame.type !in UNAUTH_ALLOWED) {
                            fatal(link, "AUTH", "frame not allowed before auth"); break@loop
                        }
                    }
                }
            }
        } catch (e: Throwable) {
            // 单连接故障不拖垮服务（SupervisorJob）；兜底吞掉占位模块抛出的 NotImplementedError 等
            Log.w(TAG, "连接处理异常：${e.javaClass.simpleName}: ${e.message}")
            // PROTOCOL §5.6：严重错误发 ERR{INTERNAL} 后关闭（尽力而为，链路可能已死）
            runCatching { fatal(link, "INTERNAL", e.message ?: "internal error") }
        } finally {
            links.remove(link)
            if (!link.authed) unauthCount.decrementAndGet()
            val wasAuthed = authedLink.get() === link
            if (wasAuthed) {
                authedLink.set(null)
                TransferEngine.handleLinkClosed(link) // 该连接上的活跃传输按隐式 CANCEL keep=true 收尾
            }
            if (ownsPairingState && _state.value is AgentState.AwaitingPairing) {
                // 该连接展示的配对码随连接断开失效
                PairingCoordinator.invalidate()
                resetToListening()
            } else if (wasAuthed && _state.value is AgentState.Paired) {
                resetToListening()
            }
            link.closeQuietly()
        }
    }

    /** AUTH 处理结果：failures=累计 AUTH+PAIR 失败数；pairingStarted=已发送 NEED_PAIR 进入配对展示。 */
    private class AuthResult(val failures: Int, val pairingStarted: Boolean)

    private suspend fun handleAuth(link: Link, ip: String, frame: Frame, currentFailures: Int): AuthResult {
        // 单已认证 PC 会话：第 2 个认证请求 → ERR BUSY（§1）
        if (authedLink.get() != null && authedLink.get() !== link) {
            fatal(link, "BUSY", "another PC session is active")
            return AuthResult(currentFailures, false)
        }
        val obj = runCatching { frame.jsonObject() }.getOrNull()
        if (obj == null) {
            fatal(link, "PROTOCOL", "bad AUTH json")
            return AuthResult(currentFailures, false)
        }
        val tokenId = obj.optString("token_id")
        val token = obj.optString("token")
        if (tokenId == null || token == null ||
            !Regex("^[0-9a-f]{8}$").matches(tokenId) || !Regex("^[A-Za-z0-9_-]{43}$").matches(token)
        ) {
            recordIpFailure(ip)
            fatal(link, "PROTOCOL", "malformed AUTH")
            return AuthResult(currentFailures, false)
        }

        val storedSha = store?.tokenSha256(tokenId)
        val matched = storedSha != null && MessageDigest.isEqual(
            storedSha.toByteArray(Charsets.US_ASCII),
            Digests.sha256Hex(token).toByteArray(Charsets.US_ASCII),
        )
        if (matched) {
            if (markAuthed(link)) {
                link.send(Protocol.T_AUTH_OK, MiniJson.obj("name" to Build.MODEL))
                PairingCoordinator.invalidate()
            } else {
                fatal(link, "BUSY", "another PC session is active")
            }
            return AuthResult(currentFailures, false)
        }

        // 校验失败（计入限速）→ NEED_PAIR，进入配对流程（AUTH 10s 计时挂起）
        recordIpFailure(ip)
        val failures = currentFailures + 1
        if (failures >= MAX_CONN_AUTH_FAILURES) return AuthResult(failures, false)

        val session = PairingCoordinator.beginSession(System.currentTimeMillis())
        link.send(Protocol.T_NEED_PAIR, MiniJson.obj("device" to Build.MODEL))
        _state.value = AgentState.AwaitingPairing(link.peerHost, session.code, session.expiresAtEpochMs)
        return AuthResult(failures, true)
    }

    private fun markAuthed(link: Link): Boolean {
        // 单会话占位失败（并发配对竞态）→ 调用方回 ERR BUSY
        if (!authedLink.compareAndSet(null, link)) return false
        link.authed = true
        unauthCount.decrementAndGet()
        _state.value = AgentState.Paired(link.peerHost, link.peerName)
        return true
    }

    private fun resetToListening() {
        if (running) _state.value = AgentState.Listening(localIpv4(), Protocol.PORT)
    }

    // ------------------------------------------------------------------
    //  已认证命令分发
    // ------------------------------------------------------------------

    private suspend fun dispatchCommand(link: Link, frame: Frame) {
        when (frame.type) {
            Protocol.T_LIST_REQ -> {
                val files = TransferEngine.requireDeps().repo.queuedForPc().map { r ->
                    MiniJson.obj(
                        "id" to r.id.toString(), "name" to r.fileName,
                        "size" to r.sizeBytes, "mime" to (r.mimeType ?: "application/octet-stream"),
                    )
                }
                link.send(Protocol.T_LIST_RSP, MiniJson.obj("files" to files))
            }

            Protocol.T_RECV_REQ -> {
                val obj = runCatching { frame.jsonObject() }.getOrElse {
                    fatal(link, "PROTOCOL", "bad RECV_REQ json"); return
                }
                val result = TransferEngine.handleRecvReq(link, obj.optString("id"), obj.optString("name"))
                if (!result.started) {
                    // NOT_FOUND 哨兵：xid 固定 0 + ref 原样回显（§5.2）
                    link.send(
                        Protocol.T_SEND_ACK,
                        MiniJson.obj(
                            "xid" to 0L, "accept" to false,
                            "reason" to result.reason, "ref" to result.ref,
                        ),
                    )
                }
            }

            Protocol.T_SEND_REQ -> {
                val obj = runCatching { frame.jsonObject() }.getOrElse {
                    fatal(link, "PROTOCOL", "bad SEND_REQ json"); return
                }
                val xid = obj.optLong("xid")
                val decision = TransferEngine.handleSendReq(
                    link,
                    xid = xid?.toInt() ?: 0,
                    name = obj.optString("name"),
                    size = obj.optLong("size"),
                    mime = obj.optString("mime"),
                    md5 = obj.optString("md5"),
                )
                val body = when (decision) {
                    is TransferEngine.AckDecision.Accepted -> MiniJson.obj(
                        "xid" to (xid ?: 0L), "accept" to true, "offset" to decision.offset,
                    )
                    is TransferEngine.AckDecision.Rejected -> MiniJson.obj(
                        "xid" to (xid ?: 0L), "accept" to false, "reason" to decision.reason,
                    )
                }
                link.send(Protocol.T_SEND_ACK, body)
            }

            Protocol.T_DATA -> {
                val payload = frame.payload
                if (payload.size < 4) { fatal(link, "PROTOCOL", "DATA without xid"); return }
                if (payload.size - 4 > Protocol.MAX_DATA_CHUNK) {
                    fatal(link, "PROTOCOL", "DATA chunk exceeds 1MiB"); return
                }
                val xid = ((payload[0].toInt() and 0xFF) shl 24) or
                    ((payload[1].toInt() and 0xFF) shl 16) or
                    ((payload[2].toInt() and 0xFF) shl 8) or
                    (payload[3].toInt() and 0xFF)
                TransferEngine.handleData(xid, payload.copyOfRange(4, payload.size))
            }

            Protocol.T_PROGRESS -> {
                val obj = runCatching { frame.jsonObject() }.getOrElse { return }
                val xid = obj.optLong("xid")?.toInt() ?: return
                TransferEngine.handleProgress(xid, obj.optLong("received") ?: return)
            }

            Protocol.T_TRANSFER_DONE -> {
                val obj = runCatching { frame.jsonObject() }.getOrElse { return }
                val xid = obj.optLong("xid")?.toInt() ?: return
                TransferEngine.handleTransferDone(xid, obj.optString("status") ?: "IO_ERROR", obj.optString("md5"))
            }

            Protocol.T_CANCEL -> {
                val obj = runCatching { frame.jsonObject() }.getOrElse { return }
                val xid = obj.optLong("xid")?.toInt() ?: return
                TransferEngine.handleCancel(xid, obj.optBoolean("keep") ?: true, obj.optString("reason"))
            }
        }
    }

    /** ERR 后立即 close（§5.6）。 */
    private suspend fun fatal(link: Link, code: String, message: String) {
        runCatching {
            link.send(Protocol.T_ERR, MiniJson.obj("code" to code, "message" to message))
        }
        link.closeQuietly()
    }

    // ------------------------------------------------------------------
    //  同 IP 限速（§3.1：AUTH 与 PAIR 校验失败合并计数）
    // ------------------------------------------------------------------

    private fun recordIpFailure(ip: String) {
        if (ip.isEmpty()) return
        val now = System.currentTimeMillis()
        synchronized(ipFailures) {
            val list = ipFailures.getOrPut(ip) { mutableListOf() }
            list.add(now)
            list.removeAll { it < now - Protocol.IP_RATE_WINDOW_MS }
            if (list.count { it >= now - Protocol.IP_RATE_WINDOW_MS } >= Protocol.IP_RATE_MAX_FAILURES) {
                synchronized(cooldownUntil) {
                    cooldownUntil[ip] = now + Protocol.IP_COOLDOWN_MS
                }
                list.clear()
                Log.w(TAG, "IP $ip 触发冷却 ${Protocol.IP_COOLDOWN_MS}ms")
            }
        }
    }

    // ------------------------------------------------------------------
    //  mDNS 注册（§2）
    // ------------------------------------------------------------------

    private fun registerNsd(context: Context, store: PairingStore) {
        val nsd = runCatching {
            context.getSystemService(Context.NSD_SERVICE) as NsdManager
        }.getOrNull() ?: return
        nsdManager = nsd
        val awaiting = PairingCoordinator.activeSession(System.currentTimeMillis()) != null
        val info = NsdServiceInfo().apply {
            serviceName = "ScreenLink-${store.instanceId.takeLast(4)}"
            serviceType = "${Protocol.MDNS_SERVICE_TYPE}."
            port = Protocol.PORT
            setTxtSafe(Protocol.TXT_PV, Protocol.VERSION.toString())
            setTxtSafe(Protocol.TXT_ID, store.instanceId)
            setTxtSafe(Protocol.TXT_NM, Build.MODEL ?: "Android")
            setTxtSafe(Protocol.TXT_PA, if (awaiting) "1" else "0")
        }
        runCatching { nsd.registerService(info, NsdManager.PROTOCOL_DNS_SD, nsdListener) }
            .onFailure { Log.w(TAG, "mDNS registerService 失败", it) }
    }

    /** NsdManager 要求 TXT 值为可打印 ASCII；非法值（如部分设备型号名）跳过该键。 */
    private fun NsdServiceInfo.setTxtSafe(key: String, value: String) {
        runCatching { setAttribute(key, value) }
            .onFailure { Log.w(TAG, "TXT $key 值非法，已跳过: ${it.message}") }
    }

    private fun unregisterNsd() {
        val nsd = nsdManager ?: return
        nsdManager = null
        runCatching { nsd.unregisterService(nsdListener) }
    }

    // ------------------------------------------------------------------
    //  连接封装
    // ------------------------------------------------------------------

    private class Link(
        val socket: Socket,
        override val peerHost: String,
    ) : TransferEngine.TransferLink {
        val input = BufferedInputStream(socket.getInputStream(), 256 * 1024)
        private val output = BufferedOutputStream(socket.getOutputStream(), 256 * 1024)
        private val writeMutex = Mutex()

        @Volatile override var peerName: String = "PC"
        @Volatile var authed = false

        override val isAlive: Boolean
            get() = !socket.isClosed && authed

        override suspend fun send(type: Int, jsonBody: Any?) {
            sendRaw(type, MiniJson.write(jsonBody).toByteArray(Charsets.UTF_8))
        }

        override suspend fun sendRaw(type: Int, payload: ByteArray) {
            writeMutex.withLock {
                withContext(Dispatchers.IO) {
                    FrameCodec.write(Frame(type, payload), output)
                    output.flush()
                }
            }
        }

        fun closeQuietly() {
            runCatching { socket.close() }
        }
    }

    private fun Socket.closeQuietly() {
        runCatching { close() }
    }

    private const val SOCKET_POLL_MS = 5_000
    private const val MAX_CONN_AUTH_FAILURES = 5 // 单连接 AUTH+PAIR 合并 5 次 → close（§7）
}
