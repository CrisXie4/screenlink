package dev.screenlink.companion.core

import dev.screenlink.companion.core.MiniJson.optBoolean
import dev.screenlink.companion.core.MiniJson.optLong
import dev.screenlink.companion.core.MiniJson.optString
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import java.io.ByteArrayInputStream
import java.io.ByteArrayOutputStream
import java.io.EOFException

/**
 * A2 单测：帧编解码字节级一致（PROTOCOL.md §4 十六进制样例逐字节比对）+ MiniJson 行为。
 * 纯 JVM（无 android.* 依赖），`gradle :app:testDebugUnitTest` 可直接跑。
 */
class FrameCodecTest {

    private fun bytes(vararg v: Int) = ByteArray(v.size) { v[it].toByte() }

    @Test
    fun `HELLO 帧 与 PROTOCOL §4 十六进制样例逐字节一致`() {
        val body = MiniJson.obj(
            "app" to "screenlink-pc/0.1.0",
            "name" to "DEV-PC",
            "pv" to 1L,
            "role" to "pc",
        )
        val out = ByteArrayOutputStream()
        FrameCodec.write(Frame.json(Protocol.T_HELLO, body), out)
        val all = out.toByteArray()

        val expectedPayload = """{"app":"screenlink-pc/0.1.0","name":"DEV-PC","pv":1,"role":"pc"}"""
        assertEquals(64, all.size - 12) // 样例 payload 64 字节 = 0x40
        assertArrayEquals(
            bytes(0x53, 0x4C, 0x4E, 0x4B, 0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x40),
            all.copyOfRange(0, 12),
        )
        assertEquals(expectedPayload, String(all, 12, all.size - 12, Charsets.UTF_8))
        // 帧总长 12 + 64 = 76（样例）
        assertEquals(76, all.size)
    }

    @Test
    fun `DATA 帧头 length 字段 = xid 4B + 数据块（262144 字节）`() {
        val chunk = ByteArray(262144)
        val payload = ByteArray(4 + chunk.size)
        payload[0] = 0; payload[1] = 0; payload[2] = 0; payload[3] = 42 // xid BE
        chunk.copyInto(payload, 4)

        val out = ByteArrayOutputStream()
        FrameCodec.write(Frame(Protocol.T_DATA, payload), out)
        val all = out.toByteArray()

        // length = 4 + 262144 = 262148 = 0x00040004。
        // 注：PROTOCOL.md §4 样例写 0x0003FFF4（= 262132）系文档算术笔误（多减了 12 帧头）；
        // 按 §4 字段定义"length = payload 字节数，不含 12 字节帧头"，正确值即 0x00040004。
        assertArrayEquals(
            bytes(0x53, 0x4C, 0x4E, 0x4B, 0x01, 0x22, 0x00, 0x00, 0x00, 0x04, 0x00, 0x04),
            all.copyOfRange(0, 12),
        )
    }

    @Test
    fun `write 后 read 往返一致（含 0 长与 4MiB 边界 payload）`() {
        for (size in intArrayOf(0, 1, 1000, Protocol.MAX_PAYLOAD)) {
            val payload = ByteArray(size) { (it % 251).toByte() }
            val out = ByteArrayOutputStream()
            FrameCodec.write(Frame(0x55, payload), out)
            val frame = FrameCodec.read(ByteArrayInputStream(out.toByteArray()))!!
            assertEquals(0x55, frame.type)
            assertArrayEquals(payload, frame.payload)
        }
    }

    @Test
    fun `未知帧类型且 version=1 正常读回（由调用方忽略）`() {
        val out = ByteArrayOutputStream()
        FrameCodec.write(Frame(0x7F, byteArrayOf(1, 2, 3)), out)
        val frame = FrameCodec.read(ByteArrayInputStream(out.toByteArray()))!!
        assertEquals(0x7F, frame.type)
        assertArrayEquals(byteArrayOf(1, 2, 3), frame.payload)
    }

    @Test
    fun `magic 不符 抛 ProtocolError`() {
        val out = ByteArrayOutputStream()
        FrameCodec.write(Frame(0x01, ByteArray(4)), out)
        val all = out.toByteArray()
        all[0] = 'X'.code.toByte()
        try {
            FrameCodec.read(ByteArrayInputStream(all))
            throw AssertionError("expected ProtocolError")
        } catch (e: FrameCodec.ProtocolError) {
            assertTrue(e.message!!.contains("magic"))
        }
    }

    @Test
    fun `version 不为 1 抛 ProtocolError`() {
        val out = ByteArrayOutputStream()
        FrameCodec.write(Frame(0x01, ByteArray(4)), out)
        val all = out.toByteArray()
        all[4] = 2
        try {
            FrameCodec.read(ByteArrayInputStream(all))
            throw AssertionError("expected ProtocolError")
        } catch (e: FrameCodec.ProtocolError) {
            assertTrue(e.message!!.contains("version"))
        }
    }

    @Test
    fun `payload 超 4MiB 抛 PayloadTooLarge`() {
        val header = bytes(0x53, 0x4C, 0x4E, 0x4B, 0x01, 0x01, 0x00, 0x00)
        val len = Protocol.MAX_PAYLOAD + 1 // 0x00400001
        val all = header + byteArrayOf(
            ((len ushr 24) and 0xFF).toByte(), ((len ushr 16) and 0xFF).toByte(),
            ((len ushr 8) and 0xFF).toByte(), (len and 0xFF).toByte(),
        )
        try {
            FrameCodec.read(ByteArrayInputStream(all))
            throw AssertionError("expected PayloadTooLarge")
        } catch (e: FrameCodec.PayloadTooLarge) {
            assertEquals(len, e.length)
        }
    }

    @Test
    fun `帧边界干净 EOF 返回 null；帧中途截断 抛 EOFException`() {
        assertNull(FrameCodec.read(ByteArrayInputStream(ByteArray(0))))
        val out = ByteArrayOutputStream()
        FrameCodec.write(Frame(0x01, "abc".toByteArray()), out)
        val all = out.toByteArray()
        try {
            FrameCodec.read(ByteArrayInputStream(all.copyOfRange(0, all.size - 1)))
            throw AssertionError("expected EOFException")
        } catch (e: EOFException) {
            // 预期
        }
    }

    @Test
    fun `jsonObject 解析 payload；非对象 payload 抛 ProtocolError`() {
        val frame = Frame.json(Protocol.T_PING, MiniJson.obj())
        assertEquals(0, frame.jsonObject().size)
        try {
            Frame(Protocol.T_PING, "[1,2]".toByteArray()).jsonObject()
            throw AssertionError("expected ProtocolError")
        } catch (e: FrameCodec.ProtocolError) {
            // 预期
        }
    }

    // ---- MiniJson ----

    @Test
    fun `字符串转义往返（引号 反斜杠 控制字符 中文）`() {
        val original = "quote\" back\\slash\nnewline\ttab\bbell 中文🚀"
        val json = MiniJson.write(original)
        assertTrue(json.startsWith("\"") && json.endsWith("\""))
        assertEquals(original, MiniJson.parse(json))
    }

    @Test
    fun `控制字符 0x01 写为 u0001 短转义`() {
        val json = MiniJson.write("\u0001")
        assertEquals("\"\\u0001\"", json)
        assertEquals("\u0001", MiniJson.parse(json))
    }

    @Test
    fun `解析器支持对象嵌套 数组 空白与字面量`() {
        val value = MiniJson.parse(""" { "a" : 1 , "b" : [true, false, null] , "c" : { "d" : -2.5 } } """)
        @Suppress("UNCHECKED_CAST")
        val obj = value as Map<String, Any?>
        assertEquals(1L, obj["a"])
        assertEquals(listOf(true, false, null), obj["b"])
        @Suppress("UNCHECKED_CAST")
        val c = obj["c"] as Map<String, Any?>
        assertEquals(-2.5, c["d"])
    }

    @Test
    fun `解析标准转义（斜杠 f 与 uXXXX）`() {
        assertEquals("/\u000CA", MiniJson.parse("\"\\/\\f\\u0041\""))
    }

    @Test
    fun `opt 系列读取辅助（容忍 1_0 形式的整数）`() {
        val obj = MiniJson.parse("""{"s":"x","i":7,"f":7.0,"b":true,"n":null}""") as Map<*, *>
        assertEquals("x", obj.optString("s"))
        assertEquals(7L, obj.optLong("i"))
        assertEquals(7L, obj.optLong("f"))
        assertEquals(true, obj.optBoolean("b"))
        assertNull(obj.optString("n"))
        assertNull(obj.optString("missing"))
    }
}
