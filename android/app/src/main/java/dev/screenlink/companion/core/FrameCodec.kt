package dev.screenlink.companion.core

import java.io.EOFException
import java.io.IOException
import java.io.InputStream
import java.io.OutputStream

/**
 * A2：12 字节帧头编解码 + JSON payload 序列化（PROTOCOL.md §4）。
 *
 * 帧头布局：magic(4B "SLNK") version(1B =1) type(1B) flags(2B 恒 0) length(4B u32 BE，不含帧头)。
 * 多字节整数大端；字符串 UTF-8；JSON 键 snake_case、紧凑输出（无空白）。
 *
 * JSON 采用手写 [MiniJson]：协议 payload 全部是平面标量结构（LIST_RSP 的 files 数组只在
 * 发送方向需要），手写实现保证纯 JVM 单测（FrameCodecTest）不依赖 org.json 的
 * android.jar Stub（其在 local unit test 中所有方法抛 "Stub!"），也不需要额外序列化依赖。
 */
class Frame(val type: Int, val payload: ByteArray) {
    companion object {
        /** 以 JSON 对象构造 payload（紧凑序列化）。 */
        fun json(type: Int, body: Any?): Frame =
            Frame(type, MiniJson.write(body).toByteArray(Charsets.UTF_8))
    }

    /** payload 按 UTF-8 JSON 解析为对象；不是 JSON 对象时抛 [FrameCodec.ProtocolError]。 */
    fun jsonObject(): Map<*, *> {
        val value = MiniJson.parse(String(payload, Charsets.UTF_8))
        @Suppress("UNCHECKED_CAST")
        return value as? Map<*, *> ?: throw FrameCodec.ProtocolError("payload is not a JSON object")
    }
}

object FrameCodec {

    /** magic/version 不符（PROTOCOL.md §4：ERR{code:"PROTOCOL"} 后 close）。 */
    class ProtocolError(message: String) : IOException(message)

    /** payload 超 4 MiB 硬上限（ERR{code:"PAYLOAD_TOO_LARGE"} 后 close）。 */
    class PayloadTooLarge(val length: Int) : IOException("frame payload too large: $length")

    /**
     * 写一帧：12 字节帧头 + payload，缓冲成单次 `out.write(...)`。
     * 并发调用方仍应自行串行化（AgentServer 用 Mutex），单次数组写出保证帧不会被交错到一半。
     */
    fun write(frame: Frame, out: OutputStream) {
        val len = frame.payload.size
        require(len <= Protocol.MAX_PAYLOAD) {
            "payload ${len}B exceeds hard limit ${Protocol.MAX_PAYLOAD}B"
        }
        val buf = ByteArray(Protocol.HEADER_SIZE + len)
        Protocol.MAGIC.toByteArray(Charsets.US_ASCII).copyInto(buf)
        buf[4] = Protocol.VERSION.toByte()
        buf[5] = frame.type.toByte()
        buf[6] = 0 // flags 保留，v1 恒 0
        buf[7] = 0
        buf[8] = ((len ushr 24) and 0xFF).toByte()
        buf[9] = ((len ushr 16) and 0xFF).toByte()
        buf[10] = ((len ushr 8) and 0xFF).toByte()
        buf[11] = (len and 0xFF).toByte()
        frame.payload.copyInto(buf, Protocol.HEADER_SIZE)
        out.write(buf)
    }

    /**
     * 读一帧：先读 12 字节帧头并校验 magic/version/长度上限，再读 length 字节 payload。
     * - 流在帧边界干净结束 → `null`；
     * - 帧中途截断 → [java.io.EOFException]；
     * - magic 不符 / version ≠ 1 → [ProtocolError]（调用方发 ERR PROTOCOL 后 close）；
     * - length > 4 MiB → [PayloadTooLarge]（调用方发 ERR PAYLOAD_TOO_LARGE 后 close）；
     * - 未知帧类型且 version==1 → 正常返回，由调用方读掉并忽略（向前兼容，§4）。
     */
    fun read(input: InputStream): Frame? {
        val header = ByteArray(Protocol.HEADER_SIZE)
        val first = input.read()
        if (first < 0) return null
        header[0] = first.toByte()
        readFully(input, header, 1, Protocol.HEADER_SIZE - 1)

        val magic = Protocol.MAGIC.toByteArray(Charsets.US_ASCII)
        if (header[0] != magic[0] || header[1] != magic[1] ||
            header[2] != magic[2] || header[3] != magic[3]
        ) {
            throw ProtocolError("bad magic")
        }
        val version = header[4].toInt() and 0xFF
        if (version != Protocol.VERSION) {
            throw ProtocolError("unsupported frame version $version (expect ${Protocol.VERSION})")
        }
        val length = ((header[8].toInt() and 0xFF) shl 24) or
            ((header[9].toInt() and 0xFF) shl 16) or
            ((header[10].toInt() and 0xFF) shl 8) or
            (header[11].toInt() and 0xFF)
        if (length > Protocol.MAX_PAYLOAD) throw PayloadTooLarge(length)

        val payload = ByteArray(length)
        readFully(input, payload, 0, length)
        return Frame(header[5].toInt() and 0xFF, payload)
    }

    private fun readFully(input: InputStream, buf: ByteArray, off: Int, len: Int) {
        var pos = off
        var remaining = len
        while (remaining > 0) {
            val n = input.read(buf, pos, remaining)
            if (n < 0) throw EOFException("stream truncated mid-frame")
            pos += n
            remaining -= n
        }
    }
}

/**
 * 极简 JSON：仅覆盖协议用到的结构 —— 对象（Map）、数组（List）、字符串、
 * 整数（Long）、Boolean、null。输出紧凑无空白；解析递归下降，支持标准转义
 * （`\" \\ \/ \b \f \n \r \t \uXXXX`）。
 */
object MiniJson {

    /** 构造保持插入序的 JSON 对象（键序可复现，便于与协议文档十六进制样例逐字节比对）。 */
    fun obj(vararg pairs: Pair<String, Any?>): LinkedHashMap<String, Any?> {
        val map = LinkedHashMap<String, Any?>(pairs.size)
        for ((k, v) in pairs) map[k] = v
        return map
    }

    /** 序列化为紧凑 JSON 文本。 */
    fun write(value: Any?): String {
        val sb = StringBuilder()
        writeValue(value, sb)
        return sb.toString()
    }

    /** 解析 JSON 文本；语法错误抛 [IllegalArgumentException]。 */
    fun parse(text: String): Any? {
        val p = Parser(text)
        val v = p.parseValue()
        p.skipWhitespace()
        if (!p.atEnd()) throw IllegalArgumentException("trailing content at ${p.pos}")
        return v
    }

    // ---- 读取辅助：协议字段都是 String / 整数 / Boolean，容忍 1 与 1.0 ----

    fun Map<*, *>.optString(key: String): String? = this[key] as? String

    fun Map<*, *>.optLong(key: String): Long? = when (val v = this[key]) {
        is Long -> v
        is Int -> v.toLong()
        is Double -> if (v.isFinite() && v == Math.floor(v) && !v.isInfinite()) v.toLong() else null
        else -> null
    }

    fun Map<*, *>.optBoolean(key: String): Boolean? = this[key] as? Boolean

    // ---- 序列化 ----

    private fun writeValue(value: Any?, sb: StringBuilder) {
        when (value) {
            null -> sb.append("null")
            is String -> writeString(value, sb)
            is Boolean -> sb.append(if (value) "true" else "false")
            is Int -> sb.append(value.toString())
            is Long -> sb.append(value.toString())
            is Double -> {
                require(value.isFinite()) { "non-finite number not representable in JSON" }
                sb.append(value.toString())
            }
            is Float -> writeValue(value.toDouble(), sb)
            is Map<*, *> -> {
                sb.append('{')
                var first = true
                for ((k, v) in value) {
                    if (!first) sb.append(',')
                    first = false
                    writeString(k.toString(), sb)
                    sb.append(':')
                    writeValue(v, sb)
                }
                sb.append('}')
            }
            is Iterable<*> -> {
                sb.append('[')
                var first = true
                for (v in value) {
                    if (!first) sb.append(',')
                    first = false
                    writeValue(v, sb)
                }
                sb.append(']')
            }
            else -> throw IllegalArgumentException("unsupported JSON value type: ${value::class.java}")
        }
    }

    private fun writeString(s: String, sb: StringBuilder) {
        sb.append('"')
        for (c in s) {
            when (c) {
                '"' -> sb.append("\\\"")
                '\\' -> sb.append("\\\\")
                '\n' -> sb.append("\\n")
                '\r' -> sb.append("\\r")
                '\t' -> sb.append("\\t")
                '\b' -> sb.append("\\b")
                else ->
                    // 0x0C(换页)与其余控制字符统一走 \u00xx 转义（JSON 合法，解析端兼容 \f 短转义）
                    if (c < ' ') sb.append("\\u").append(String.format("%04x", c.code))
                    else sb.append(c)
            }
        }
        sb.append('"')
    }

    // ---- 解析 ----

    private class Parser(val text: String) {
        var pos = 0

        fun atEnd(): Boolean = pos >= text.length

        fun skipWhitespace() {
            while (pos < text.length && text[pos].let { it == ' ' || it == '\t' || it == '\n' || it == '\r' }) pos++
        }

        fun parseValue(): Any? {
            skipWhitespace()
            if (atEnd()) throw IllegalArgumentException("unexpected end of JSON")
            return when (text[pos]) {
                '{' -> parseObject()
                '[' -> parseArray()
                '"' -> parseString()
                't' -> expectLiteral("true", true)
                'f' -> expectLiteral("false", false)
                'n' -> expectLiteral("null", null)
                else -> parseNumber()
            }
        }

        private fun expectLiteral(literal: String, value: Any?): Any? {
            if (!text.startsWith(literal, pos)) throw IllegalArgumentException("bad literal at $pos")
            pos += literal.length
            return value
        }

        private fun parseObject(): LinkedHashMap<String, Any?> {
            pos++ // '{'
            val map = LinkedHashMap<String, Any?>()
            skipWhitespace()
            if (!atEnd() && text[pos] == '}') { pos++; return map }
            while (true) {
                skipWhitespace()
                if (atEnd() || text[pos] != '"') throw IllegalArgumentException("expected string key at $pos")
                val key = parseString()
                skipWhitespace()
                if (atEnd() || text[pos] != ':') throw IllegalArgumentException("expected ':' at $pos")
                pos++
                map[key] = parseValue()
                skipWhitespace()
                if (atEnd()) throw IllegalArgumentException("unterminated object")
                when (text[pos]) {
                    ',' -> pos++
                    '}' -> { pos++; return map }
                    else -> throw IllegalArgumentException("expected ',' or '}' at $pos")
                }
            }
        }

        private fun parseArray(): ArrayList<Any?> {
            pos++ // '['
            val list = ArrayList<Any?>()
            skipWhitespace()
            if (!atEnd() && text[pos] == ']') { pos++; return list }
            while (true) {
                list.add(parseValue())
                skipWhitespace()
                if (atEnd()) throw IllegalArgumentException("unterminated array")
                when (text[pos]) {
                    ',' -> pos++
                    ']' -> { pos++; return list }
                    else -> throw IllegalArgumentException("expected ',' or ']' at $pos")
                }
            }
        }

        fun parseString(): String {
            pos++ // '"'
            val sb = StringBuilder()
            while (true) {
                if (atEnd()) throw IllegalArgumentException("unterminated string")
                when (val c = text[pos]) {
                    '"' -> { pos++; return sb.toString() }
                    '\\' -> {
                        pos++
                        if (atEnd()) throw IllegalArgumentException("unterminated escape")
                        when (val e = text[pos]) {
                            '"' -> sb.append('"')
                            '\\' -> sb.append('\\')
                            '/' -> sb.append('/')
                            'b' -> sb.append('\b')
                            'f' -> sb.append(12.toChar())
                            'n' -> sb.append('\n')
                            'r' -> sb.append('\r')
                            't' -> sb.append('\t')
                            'u' -> {
                                if (pos + 4 >= text.length) throw IllegalArgumentException("bad \\u escape at $pos")
                                val hex = text.substring(pos + 1, pos + 5)
                                sb.append(hex.toInt(16).toChar())
                                pos += 4
                            }
                            else -> throw IllegalArgumentException("bad escape '\\$e' at $pos")
                        }
                        pos++
                    }
                    else -> {
                        if (c.code < 0x20) throw IllegalArgumentException("raw control char in string at $pos")
                        sb.append(c)
                        pos++
                    }
                }
            }
        }

        private fun parseNumber(): Any {
            val start = pos
            if (!atEnd() && (text[pos] == '-' || text[pos] == '+')) pos++
            var isDouble = false
            while (!atEnd()) {
                val c = text[pos]
                if (c in '0'..'9') pos++
                else if (c == '.' || c == 'e' || c == 'E' || c == '+' || c == '-') {
                    isDouble = isDouble || c == '.' || c == 'e' || c == 'E'
                    pos++
                } else break
            }
            val token = text.substring(start, pos)
            if (token.isEmpty()) throw IllegalArgumentException("bad number at $start")
            return if (isDouble) token.toDouble()
            else token.toLongOrNull() ?: throw IllegalArgumentException("number out of range: $token")
        }
    }
}
