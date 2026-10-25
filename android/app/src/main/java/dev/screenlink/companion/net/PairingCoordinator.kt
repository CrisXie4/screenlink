package dev.screenlink.companion.net

import dev.screenlink.companion.core.Protocol
import dev.screenlink.companion.transfer.Digests
import java.security.MessageDigest
import java.security.SecureRandom
import java.util.Base64
import javax.crypto.Mac
import javax.crypto.spec.SecretKeySpec

/**
 * A4：配对码会话与 token 派生（PROTOCOL.md §3.1/§3.3，决策 D3）。
 *
 * 配对码规则：6 位十进制、CSPRNG、TTL 300s、单次使用、连续 5 次校验失败立即作废。
 * token 派生（精确定义 §3.3）：
 * ```
 * token   = base64url_nopad( HKDF-SHA256(ikm=seed, salt=ASCII("screenlink/v1/pair/"+code), info=ASCII("token"), L=32) )
 * token_id  = lower_hex( SHA-256(ASCII(token)) )[0..8]
 * 手机存储    = SHA-256(ASCII(token))（64 hex，经 PairingStore）
 * ```
 * 6 位码仅承担一次性在线交换门槛；token 熵由 32B CSPRNG 种子保证（256 位）。
 * 校验比较一律 [MessageDigest.isEqual] 常量时间实现（§10）。
 *
 * 本类为纯 JVM 逻辑（不触 Android API），单会话模型：同一时刻至多一个有效配对码。
 */
object PairingCoordinator {

    /** 一次配对会话：码 + 派生物。派生物不出手机，仅在 PAIR 通过后经 AgentServer 下发一次。 */
    class Session internal constructor(
        val code: String,
        val expiresAtEpochMs: Long,
        internal val token: String,
        internal val tokenId: String,
        internal val tokenSha256: String,
    ) {
        @Volatile
        var failuresLeft: Int = Protocol.PAIRING_MAX_FAILURES
            private set

        /** true = 已被使用或已作废（过期/失败次数耗尽），不再是有效码。 */
        @Volatile
        var consumed: Boolean = false
            private set

        fun expired(nowEpochMs: Long): Boolean = nowEpochMs >= expiresAtEpochMs

        fun valid(nowEpochMs: Long): Boolean = !consumed && !expired(nowEpochMs)

        internal fun consume() {
            consumed = true
        }

        internal fun noteFailure() {
            failuresLeft = (failuresLeft - 1).coerceAtLeast(0)
            if (failuresLeft == 0) consumed = true
        }
    }

    sealed interface CheckResult {
        /** 校验通过；码随即作废（单次使用）。 */
        data class Accepted(val session: Session) : CheckResult

        /**
         * 校验失败；`attemptsLeft` 为剩余次数，0 表示码已作废
         * （PAIR_FAIL{attempts_left:0}，PC 需重发 AUTH 触发新配对码）。
         */
        data class Rejected(val attemptsLeft: Int) : CheckResult
    }

    /** 派生结果（PAIR_OK 帧与 PairingStore 需要的三元组）。 */
    class Derived internal constructor(
        val token: String,
        val tokenId: String,
        val tokenSha256: String,
    )

    private val random = SecureRandom()

    @Volatile
    private var current: Session? = null

    /** 生成新配对会话（替换旧会话；旧码立即作废）。 */
    @Synchronized
    fun beginSession(nowEpochMs: Long): Session {
        current?.consume()
        val code = String.format("%0${Protocol.PAIRING_CODE_LENGTH}d", random.nextInt(1_000_000))
        val seed = ByteArray(32).also(random::nextBytes)
        val derived = deriveToken(seed, code)
        val session = Session(
            code = code,
            expiresAtEpochMs = nowEpochMs + Protocol.PAIRING_CODE_TTL_MS,
            token = derived.token,
            tokenId = derived.tokenId,
            tokenSha256 = derived.tokenSha256,
        )
        current = session
        return session
    }

    /** 当前有效会话（未消费且未过期），否则 null。 */
    @Synchronized
    fun activeSession(nowEpochMs: Long): Session? =
        current?.takeIf { it.valid(nowEpochMs) }

    /** 作废当前会话（配对连接断开时调用，码随之失效）。 */
    @Synchronized
    fun invalidate() {
        current?.consume()
        current = null
    }

    /**
     * 校验 6 位码（PROTOCOL.md §3.1）。过期/已消费码一律按
     * `Rejected(attemptsLeft=0)` 处理（PC 端语义：重新触发 NEED_PAIR）。
     */
    @Synchronized
    fun check(session: Session, code: String, nowEpochMs: Long): CheckResult {
        require(session === current) { "stale pairing session" }
        if (session.consumed || session.expired(nowEpochMs)) {
            session.consume()
            return CheckResult.Rejected(0)
        }
        val matched = MessageDigest.isEqual(
            session.code.toByteArray(Charsets.US_ASCII),
            code.toByteArray(Charsets.US_ASCII),
        )
        return if (matched) {
            session.consume()
            CheckResult.Accepted(session)
        } else {
            session.noteFailure()
            CheckResult.Rejected(session.failuresLeft)
        }
    }

    /**
     * §3.3 token 派生。独立暴露便于测试与复现：
     * HKDF-SHA256(RFC 5869)：`PRK = HMAC-SHA256(salt, ikm)`；`OKM = HMAC-SHA256(PRK, info ‖ 0x01)[0..32]`。
     */
    fun deriveToken(seed: ByteArray, code: String): Derived {
        val salt = "screenlink/v1/pair/$code".toByteArray(Charsets.US_ASCII)
        val info = "token".toByteArray(Charsets.US_ASCII)
        val okm = hkdfSha256(seed, salt, info, 32)
        val token = Base64.getUrlEncoder().withoutPadding().encodeToString(okm) // 32B → 恒 43 字符
        val sha = Digests.sha256Hex(token)
        return Derived(token = token, tokenId = sha.take(8), tokenSha256 = sha)
    }

    /** RFC 5869 HKDF（SHA-256，L ≤ 255*32）。 */
    fun hkdfSha256(ikm: ByteArray, salt: ByteArray, info: ByteArray, outLen: Int): ByteArray {
        require(outLen in 1..255 * 32) { "invalid HKDF output length $outLen" }
        val prk = hmacSha256(salt, ikm)
        val okm = ByteArray(outLen)
        var t = ByteArray(0)
        var copied = 0
        var counter = 1
        while (copied < outLen) {
            val input = ByteArray(t.size + info.size + 1)
            t.copyInto(input)
            info.copyInto(input, t.size)
            input[input.size - 1] = counter.toByte()
            t = hmacSha256(prk, input)
            val n = minOf(t.size, outLen - copied)
            t.copyInto(okm, copied, 0, n)
            copied += n
            counter++
        }
        return okm
    }

    private fun hmacSha256(key: ByteArray, data: ByteArray): ByteArray {
        val mac = Mac.getInstance("HmacSHA256")
        mac.init(SecretKeySpec(key, "HmacSHA256"))
        return mac.doFinal(data)
    }
}
