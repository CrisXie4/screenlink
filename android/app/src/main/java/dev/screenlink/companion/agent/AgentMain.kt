package dev.screenlink.companion.agent

import dev.screenlink.companion.core.Frame
import dev.screenlink.companion.core.FrameCodec
import dev.screenlink.companion.core.Protocol
import java.io.File
import java.io.FileOutputStream
import java.net.InetAddress
import java.net.InetSocketAddress
import java.net.ServerSocket
import java.net.Socket
import java.security.MessageDigest

/**
 * Shell 模式 agent 入口（经 Shizuku `app_process` 启动，shell uid）。
 *
 * 为什么存在：MIUI/HyperOS 冻结后台 App 进程（cgroup freezer，Doze 白名单无效），
 * 而 shell uid 进程完全免疫——agent 以 shell 身份常驻，息屏/切后台均不影响。
 *
 * 纯 JVM + 本 APK core 包（app_process 以 APK 为 classpath，FrameCodec/MiniJson/Protocol 直接复用）。
 * 与 App 内 SAF 版 AgentServer 的差异：
 *  - 直接写公共存储（shell 有全量存储权限），无 SAF；
 *  - 收到文件后 `cmd notification post` 发系统通知；
 *  - 若前台是聊天类应用（微信/QQ/钉钉/Telegram），自动拉起其分享面板并附上文件
 *    （vivo 电脑管家式交互）。
 *
 * 用法：CLASSPATH=<apk> app_process / dev.screenlink.companion.agent.AgentMain <port> <storageDir> <tokenSha256Hex>
 */
fun main(args: Array<String>) {
    val port = args.getOrNull(0)?.toIntOrNull() ?: 27188
    val storageDir = File(args.getOrNull(1) ?: "/sdcard/Download/ScreenLink")
    // 支持逗号分隔多个 sha（历史配对 token 均有效）
    val tokenShas = (args.getOrNull(2) ?: run {
        System.err.println("用法: AgentMain <port> <storageDir> <tokenSha256Hex[,sha2,...]>")
        return
    }).split(',').map { it.trim().lowercase() }
    File(storageDir, "outbox").mkdirs()
    dev.screenlink.companion.core.DiscoveryResponder.start(
        name = android.os.Build.MODEL, serial = "", agentPort = port,
    )
    System.err.println("[sl-agent] listening :$port storage=$storageDir")

    val server = ServerSocket()
    server.setReuseAddress(true)
    server.bind(InetSocketAddress(InetAddress.getByName("0.0.0.0"), port))
    var connId = 0
    while (true) {
        val socket = try {
            server.accept()
        } catch (e: Exception) {
            System.err.println("[sl-agent] accept 失败: $e")
            continue
        }
        connId += 1
        val id = connId
        Thread({ serve(socket, storageDir, tokenShas, id) }, "sl-conn-$id").apply {
            isDaemon = false
            start()
        }
    }
}

private const val MAX_DATA = 1 shl 20
private val FORWARD_PACKAGES = listOf("com.tencent.mm", "com.tencent.mobileqq", "com.alibaba.android.rimet", "org.telegram.messenger")

private fun serve(socket: Socket, storageDir: File, tokenShas: List<String>, connId: Int) {
    val peer = try { (socket.remoteSocketAddress as? InetSocketAddress)?.address?.hostAddress } catch (_: Exception) { null }
    System.err.println("[sl-agent] [conn$connId] 连接 $peer")
    try {
        socket.tcpNoDelay = true
        socket.soTimeout = 75_000 // §7：75s 无入帧断开
        val input = socket.getInputStream()
        val output = socket.getOutputStream()

        val hello = FrameCodec.read(input) ?: return
        val helloObj = hello.jsonObject()
        if (helloObj["role"] != "pc" || helloObj["pv"] != 1L) {
            FrameCodec.write(Frame.json(Protocol.T_ERR, mapOf("code" to "PROTOCOL", "message" to "bad HELLO")), output)
            return
        }
        FrameCodec.write(
            Frame.json(Protocol.T_HELLO, mapOf("role" to "phone", "name" to deviceName(), "app" to "screenlink-agent-shell/0.1.0", "pv" to 1L)),
            output,
        )

        var authed = false
        var activeXid = -1L
        var size = 0L
        var name = ""
        var mime = "application/octet-stream"
        var expectMd5: String? = null
        var out: FileOutputStream? = null
        var part: File? = null
        var received = 0L
        var md5: MessageDigest = MessageDigest.getInstance("MD5")

        fun closeTransfer(keep: Boolean) {
            runCatching { out?.close() }
            out = null
            if (!keep) {
                part?.delete()
            }
            activeXid = -1
        }

        fun authReject() {
            FrameCodec.write(Frame.json(Protocol.T_ERR, mapOf("code" to "AUTH", "message" to "未认证或 token 不匹配")), output)
        }

        while (true) {
            val frame = try {
                FrameCodec.read(input) ?: break
            } catch (e: java.net.SocketTimeoutException) {
                System.err.println("[sl-agent] [c$connId] ${ts()} $peer 75s 无入帧")
                break
            } catch (e: java.io.EOFException) {
                System.err.println("[sl-agent] [c$connId] ${ts()} $peer EOF")
                break
            }
            System.err.println("[sl-agent] [c$connId] ${ts()} <- ${frame.type} (${frame.payload.size}B)")
            when (frame.type) {
                Protocol.T_PING -> FrameCodec.write(Frame.json(Protocol.T_PONG, emptyMap<Any, Any>()), output)
                Protocol.T_BYE -> {
                    FrameCodec.write(Frame.json(Protocol.T_BYE, mapOf("reason" to "bye")), output)
                    break
                }
                Protocol.T_AUTH -> {
                    val o = frame.jsonObject()
                    val tokenId = o["token_id"]?.toString() ?: ""
                    val token = o["token"]?.toString() ?: ""
                    if (Regex("^[0-9a-f]{8}$").matches(tokenId) &&
                        Regex("^[A-Za-z0-9_-]{43}$").matches(token) &&
                        sha256Hex(token) in tokenShas
                    ) {
                        authed = true
                        FrameCodec.write(Frame.json(Protocol.T_AUTH_OK, mapOf("name" to deviceName())), output)
                        System.err.println("[sl-agent] [conn$connId] 已认证")
                    } else {
                        // shell agent 不做配对（配对在 App 内）；格式非法或不匹配直接拒绝
                        authReject()
                        break
                    }
                }
                Protocol.T_LIST_REQ -> {
                    if (!authed) { authReject(); break }
                    val outbox = File(storageDir, "outbox")
                    val files = outbox.listFiles()?.filter { it.isFile }?.map {
                        mapOf("id" to it.name, "name" to it.name, "size" to it.length(), "mime" to "application/octet-stream")
                    } ?: emptyList()
                    FrameCodec.write(Frame.json(Protocol.T_LIST_RSP, mapOf<String, Any?>("files" to files)), output)
                }
                Protocol.T_RECV_REQ -> {
                    if (!authed) { authReject(); break }
                    val o = frame.jsonObject()
                    val want = (o["id"] ?: o["name"])?.toString() ?: ""
                    val f = File(storageDir, "outbox").resolve(want)
                    if (!f.isFile) {
                        FrameCodec.write(Frame.json(Protocol.T_SEND_ACK, mapOf("xid" to 0, "accept" to false, "reason" to "NOT_FOUND", "ref" to want)), output)
                        continue
                    }
                    val xid = ++xidCounter
                    val fileMd5 = md5OfFile(f)
                    activeXid = xid
                    size = f.length()
                    name = f.name
                    FrameCodec.write(
                        Frame.json(Protocol.T_SEND_REQ, mapOf("xid" to xid, "name" to f.name, "size" to f.length(), "mime" to "application/octet-stream", "md5" to fileMd5)),
                        output,
                    )
                    val (at, ao) = readJson(output, input)
                    if (at != Protocol.T_SEND_ACK || (ao["accept"] as? Boolean) != true) {
                        activeXid = -1
                        continue
                    }
                    var off = (ao["offset"] as? Long) ?: 0L
                    f.inputStream().use { input2 ->
                        if (off > 0) input2.skip(off)
                        val buf = ByteArray(MAX_DATA)
                        while (true) {
                            val n = input2.read(buf)
                            if (n <= 0) break
                            val payload = ByteArray(4 + n)
                            payload[0] = ((xid shr 24) and 0xFF).toByte()
                            payload[1] = ((xid shr 16) and 0xFF).toByte()
                            payload[2] = ((xid shr 8) and 0xFF).toByte()
                            payload[3] = (xid and 0xFF).toByte()
                            System.arraycopy(buf, 0, payload, 4, n)
                            FrameCodec.write(Frame(Protocol.T_DATA, payload), output)
                            off += n
                        }
                    }
                    activeXid = -1
                }
                Protocol.T_SEND_REQ -> {
                    if (!authed) { authReject(); break }
                    val o = frame.jsonObject()
                    val xid = (o["xid"] as? Long) ?: 0L
                    activeXid = xid  // 关键：登记活跃传输，否则后续 DATA 帧会被守卫丢弃
                    val n = o["name"]?.toString() ?: ""
                    if (n.isEmpty() || n.contains('/') || n.contains('\\') || n == "." || n == "..") {
                        FrameCodec.write(Frame.json(Protocol.T_SEND_ACK, mapOf("xid" to xid, "accept" to false, "reason" to "INVALID")), output)
                        continue
                    }
                    name = n
                    mime = o["mime"]?.toString() ?: "application/octet-stream"
                    size = (o["size"] as? Long) ?: 0L
                    expectMd5 = o["md5"]?.toString()
                    val partFile = File(storageDir, "$name.part")
                    val offset0 = if (partFile.exists() && partFile.length() <= size) partFile.length() else 0L
                    received = offset0
                    md5 = MessageDigest.getInstance("MD5")
                    if (offset0 > 0) md5.update(partFile.readBytes()) // 前缀 re-hash（§6.2.5）
                    out = FileOutputStream(partFile, true)
                    part = partFile
                    FrameCodec.write(Frame.json(Protocol.T_SEND_ACK, mapOf("xid" to xid, "accept" to true, "offset" to offset0)), output)
                    System.err.println("[sl-agent] [conn$connId] 接收 $name offset=$offset0 activeXid 将设为 $xid")
                }
                Protocol.T_DATA -> {
                    if (!authed || activeXid < 0) continue
                    val xid = xidOf(frame.payload)
                    val bytes = frame.payload.copyOfRange(4, frame.payload.size)
                    out?.write(bytes)
                    md5.update(bytes)
                    received += bytes.size
                    if (received >= size) {
                        runCatching { out?.close() }; out = null
                        val actual = hex(md5.digest())
                        val status = when {
                            expectMd5 != null && actual != expectMd5 -> { part?.delete(); "MD5_MISMATCH" }
                            else -> {
                                val final = File(storageDir, name)
                                if (part?.renameTo(final) == true || final.exists()) "OK" else "IO_ERROR"
                            }
                        }
                        FrameCodec.write(Frame.json(Protocol.T_TRANSFER_DONE, mapOf("xid" to xid, "status" to status, "md5" to actual)), output)
                        System.err.println("[sl-agent] 传输完成 $name ($received B) status=$status")
                        if (status == "OK") onReceived(File(storageDir, name), mime, peer)
                        activeXid = -1
                    }
                }
                Protocol.T_CANCEL -> {
                    val keep = frame.jsonObject()["keep"] as? Boolean ?: true
                    if (!keep) part?.delete()
                    runCatching { out?.close() }; out = null
                    activeXid = -1
                }
                Protocol.T_ERR -> break
            }
        }
        runCatching { out?.close() }
        System.err.println("[sl-agent] [conn$connId] $peer 连接结束")
    } catch (e: Exception) {
        System.err.println("[sl-agent] 连接异常: $e")
    }
}

private var xidCounter = 0x1000L

private fun xidOf(payload: ByteArray): Long {
    var v = 0L
    for (i in 0 until 4) v = (v shl 8) or (payload[i].toLong() and 0xFF)
    return v
}

private fun ts(): String = java.text.SimpleDateFormat("HH:mm:ss.SSS").format(java.util.Date())

private fun hex(b: ByteArray): String = b.joinToString("") { "%02x".format(it) }
private fun sha256Hex(s: String): String = hex(MessageDigest.getInstance("SHA-256").digest(s.toByteArray()))
private fun md5OfFile(f: File): String {
    val d = MessageDigest.getInstance("MD5")
    f.inputStream().use { input ->
        val buf = ByteArray(MAX_DATA)
        while (true) {
            val n = input.read(buf)
            if (n <= 0) break
            d.update(buf, 0, n)
        }
    }
    return hex(d.digest())
}

private fun deviceName(): String = runCatching {
    val p = ProcessBuilder("getprop", "ro.product.model").start()
    val name = p.inputStream.bufferedReader().readText().trim()
    p.waitFor()
    name.ifEmpty { "Android" }
}.getOrDefault("Android")

private fun exec(cmd: String): String {
    val p = ProcessBuilder("sh", "-c", cmd).start()
    val out = p.inputStream.bufferedReader().readText()
    p.errorStream.bufferedReader().readText()
    p.waitFor()
    return out
}

private fun human(n: Long): String {
    val u = listOf("B", "KB", "MB", "GB")
    var v = n.toDouble(); var i = 0
    while (v >= 1024 && i < u.size - 1) { v /= 1024; i++ }
    return "%.1f %s".format(v, u[i])
}

/** 收到文件后：系统通知 + 前台为聊天应用时自动拉起分享面板（vivo 电脑管家式）。 */
private fun onReceived(file: File, mime: String, peer: String?) {
    val sizeText = human(file.length())
    runCatching {
        exec("cmd notification post -S bigtext -t ScreenLink sl_recv \"收到 ${file.name} ($sizeText) 来自 $peer\"")
    }
    val front = frontApp() ?: return
    val shareActivity = when {
        front.startsWith("com.tencent.mm") -> "com.tencent.mm/com.tencent.mm.ui.tools.ShareFileUI"
        front.startsWith("com.tencent.mobileqq") -> "com.tencent.mobileqq/activity.QQBrowserDelegation"
        front.startsWith("com.alibaba.android.rimet") -> "com.alibaba.android.rimet.biz.SplashActivity"
        else -> return
    }
    runCatching {
        exec(
            "am start -n $shareActivity -a android.intent.action.SEND " +
                "-t \"$mime\" --eu android.intent.extra.STREAM \"file://${file.absolutePath}\"",
        )
        System.err.println("[sl-agent] 前台为 ${front.substringBefore('.')}，已拉起分享面板（${file.name}）")
    }
}

private fun frontApp(): String? = runCatching {
    val out = exec("dumpsys window")
    Regex("mCurrentFocus=Window\\{[^}]*\\s([^\\s/]+)").find(out)?.groupValues?.get(1)
}.getOrNull()

/** 读一帧并返回 (type, json)；非 JSON payload 返回空 map。 */
private fun readJson(output: java.io.OutputStream, input: java.io.InputStream): Pair<Int, Map<String, Any?>> {
    val f = FrameCodec.read(input) ?: throw java.io.EOFException("对端关闭")
    val obj = runCatching { f.jsonObject() }.getOrNull() ?: emptyMap<String, Any?>()
    @Suppress("UNCHECKED_CAST")
    return f.type to (obj as? Map<String, Any?> ?: emptyMap())
}
