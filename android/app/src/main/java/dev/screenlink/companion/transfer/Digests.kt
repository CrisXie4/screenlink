package dev.screenlink.companion.transfer

import java.io.File
import java.io.FileInputStream
import java.io.InputStream
import java.security.MessageDigest

/**
 * A12：流式摘要工具（PROTOCOL.md §6.3：传输校验用 MD5，32 位小写 hex；
 * §3.3：token_id / token 摘要用 SHA-256）。
 */
object Digests {
    private const val BUFFER = 64 * 1024

    /** 流式计算 stream 的 MD5，返回 32 位小写 hex。读完不关闭流（调用方管理生命周期）。 */
    fun md5Of(stream: InputStream): String = digestOf("MD5", stream)

    /** 文件版 md5Of。 */
    fun md5Of(file: File): String = FileInputStream(file).use { md5Of(it) }

    /** 文本 SHA-256，返回 64 位小写 hex（用于 PairingStore 的 token 摘要与 token_id 派生）。 */
    fun sha256Hex(text: String): String {
        val md = MessageDigest.getInstance("SHA-256")
        return toHex(md.digest(text.toByteArray(Charsets.US_ASCII)))
    }

    /** 字节数组 → 小写 hex。 */
    fun toHex(bytes: ByteArray): String {
        val sb = StringBuilder(bytes.size * 2)
        for (b in bytes) {
            val v = b.toInt() and 0xFF
            sb.append("0123456789abcdef"[v ushr 4])
            sb.append("0123456789abcdef"[v and 0x0F])
        }
        return sb.toString()
    }

    private fun digestOf(algorithm: String, stream: InputStream): String {
        val md = MessageDigest.getInstance(algorithm)
        val buf = ByteArray(BUFFER)
        while (true) {
            val n = stream.read(buf)
            if (n < 0) break
            if (n > 0) md.update(buf, 0, n)
        }
        return toHex(md.digest())
    }

    /**
     * 增量器：接收方向续传时对已有前缀 re-hash 后，与新收字节滚动合并成整文件 MD5
     * （PROTOCOL.md §6.2.4/6.2.5），无需结束时重读全文件。
     */
    class Incremental(md: MessageDigest = MessageDigest.getInstance("MD5")) {
        private val md = md
        fun update(bytes: ByteArray, offset: Int = 0, len: Int = bytes.size) {
            md.update(bytes, offset, len)
        }

        /** 终结并输出 32 位小写 hex（MessageDigest.digest() 自动复位，本实例随后可继续 update）。 */
        fun hex(): String = toHex(md.digest())
    }
}
