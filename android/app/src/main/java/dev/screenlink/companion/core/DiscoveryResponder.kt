package dev.screenlink.companion.core

import java.net.DatagramPacket
import java.net.DatagramSocket
import java.net.InetSocketAddress

/**
 * UDP 广播发现应答器（PROTOCOL.md §2）：
 * PC 广播 `SCREENLINK_DISCOVER_V1` 到 UDP 27186，本端回
 * `{"name":"<机型>","serial":"<稳定id可为空>","ip":"","port":27188}`。
 *
 * 纯 JVM 实现（DatagramSocket + 守护线程），App 内置 server 与 shell agent 共用；
 * 回复的 ip 留空，PC 端用收到包的源地址填充（避免 NAT/多网卡取错地址）。
 * 端口被占（重复启动/多实例）时静默退出——发现是辅助路径，不阻塞主链路。
 */
object DiscoveryResponder {
    private const val TAG = "DiscoveryResponder"

    @Volatile private var thread: Thread? = null

    fun start(name: String, serial: String, agentPort: Int = Protocol.PORT) {
        if (thread?.isAlive == true) return
        thread = Thread({
            try {
                val socket = DatagramSocket(null)
                socket.reuseAddress = true
                socket.bind(InetSocketAddress(Protocol.UDP_DISCOVER_PORT))
                val buf = ByteArray(64)
                while (!Thread.currentThread().isInterrupted) {
                    val pkt = DatagramPacket(buf, buf.size)
                    socket.receive(pkt)
                    val msg = String(buf, 0, pkt.length, Charsets.US_ASCII)
                    if (msg != Protocol.UDP_DISCOVER_MAGIC) continue
                    val json = "{\"name\":\"${jesc(name)}\",\"serial\":\"${jesc(serial)}\"," +
                        "\"ip\":\"\",\"port\":$agentPort}"
                    val out = json.toByteArray(Charsets.UTF_8)
                    socket.send(DatagramPacket(out, out.size, pkt.address, pkt.port))
                }
            } catch (_: Throwable) {
                // 绑定失败/中断：发现路径降级为不可发现，不影响 TCP 主链路
            }
        }, "sl-discover").apply { isDaemon = true; start() }
    }

    fun stop() {
        thread?.interrupt()
        thread = null
    }

    private fun jesc(s: String) = s.replace("\\", "\\\\").replace("\"", "\\\"").replace("\n", "")
}
