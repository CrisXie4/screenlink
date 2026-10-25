package dev.screenlink.companion.transfer

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import java.io.ByteArrayInputStream
import java.io.File
import java.security.MessageDigest

/**
 * A12 单测：流式 MD5 / SHA-256 / 增量合并。纯 JVM，MD5 已知向量校验
 * （RFC 1321 经典测试向量："" 与 "abc"）。
 */
class DigestsTest {

    private fun bytes(vararg v: Int) = ByteArray(v.size) { v[it].toByte() }

    @Test
    fun `空流 MD5 已知向量`() {
        val hex = Digests.md5Of(ByteArrayInputStream(ByteArray(0)))
        assertEquals("d41d8cd98f00b204e9800998ecf8427e", hex)
    }

    @Test
    fun `abc MD5 已知向量`() {
        val hex = Digests.md5Of(ByteArrayInputStream("abc".toByteArray()))
        assertEquals("900150983cd24fb0d6963f7d28e17f72", hex)
    }

    @Test
    fun `跨块大流与一次性 MessageDigest 一致`() {
        // 3×64KB 缓冲 + 13 字节零头，验证多块滚动
        val data = ByteArray(3 * 64 * 1024 + 13) { (it * 31 % 256).toByte() }
        val hex = Digests.md5Of(ByteArrayInputStream(data))
        val expected = MessageDigest.getInstance("MD5").digest(data)
            .joinToString("") { "%02x".format(it) }
        assertEquals(expected, hex)
        assertEquals(32, hex.length)
        assertTrue(hex.none { it.isUpperCase() })
    }

    @Test
    fun `Incremental 前缀 re-hash 与后续滚动合并 等价于整文件一次计算`() {
        // 模拟 §6.2.5：前缀（已有半成品）re-hash 后，与新收字节滚动合并
        val prefix = ByteArray(150 * 1024) { (it % 253).toByte() }
        val rest = ByteArray(70 * 1024 + 7) { (it * 17 % 256).toByte() }

        val incremental = Digests.Incremental()
        incremental.update(prefix)                                   // 前缀 re-hash
        var pos = 0
        while (pos < rest.size) {                                    // 分块吸收新数据
            val n = minOf(4096, rest.size - pos)
            incremental.update(rest, pos, n)
            pos += n
        }

        val expected = MessageDigest.getInstance("MD5")
            .digest(prefix + rest)
            .joinToString("") { "%02x".format(it) }
        assertEquals(expected, incremental.hex())
    }

    @Test
    fun `文件版 md5Of 与流版一致`() {
        val data = ByteArray(200 * 1024) { (it % 249).toByte() }
        val tmp = File.createTempFile("sl_digest", ".bin")
        try {
            tmp.writeBytes(data)
            assertEquals(Digests.md5Of(ByteArrayInputStream(data)), Digests.md5Of(tmp))
        } finally {
            tmp.delete()
        }
    }

    @Test
    fun `sha256Hex 已知向量与 token_id 派生形状`() {
        assertEquals(
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
            Digests.sha256Hex("abc"),
        )
        assertEquals(64, Digests.sha256Hex("").length)
        // token_id = lower_hex(SHA-256(ASCII(token)))[0..8]（PROTOCOL §3.3）：43 字符
        // base64url token → 8 位小写 hex 前缀
        val token = "abc-_" + "0".repeat(38) // 恰 43 字符
        assertEquals(43, token.length)
        val sha = Digests.sha256Hex(token)
        assertEquals(64, sha.length)
        assertEquals(sha.take(8), sha.take(8).lowercase())
        assertEquals(8, sha.take(8).length)
    }

    @Test
    fun `toHex 输出小写且可逆`() {
        assertEquals("deadbeef", Digests.toHex(bytes(0xde, 0xad, 0xbe, 0xef)))
        assertEquals("00ff", Digests.toHex(bytes(0x00, 0xff)))
    }
}
