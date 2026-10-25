package dev.screenlink.companion.net

import android.content.Context
import android.content.SharedPreferences
import dev.screenlink.companion.transfer.Digests
import java.security.SecureRandom

/**
 * A3：token 持久化（DESIGN.md §4-A3 / PROTOCOL.md §3.1）。
 *
 * 只存 `token_id → SHA-256(token) hex` 映射（校验时对 PC 送来的明文 token 现算摘要再做
 * 常量时间比较），**永不存明文 token**——明文只在 PC 侧。另持久化设备随机实例 id
 * （8 位小写 hex，首次运行生成，mDNS TXT `id` 与 PC 侧 token 绑定校验都依赖它）。
 * SharedPreferences MODE_PRIVATE；manifest `allowBackup=false` 保证不进云备份。
 */
class PairingStore(context: Context) {

    private val prefs: SharedPreferences =
        context.applicationContext.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)

    private val random = SecureRandom()

    /**
     * 持久实例 id：8 位小写 hex。已存的值格式非法（被篡改/旧版本）时重新生成。
     */
    val instanceId: String by lazy {
        val stored = prefs.getString(KEY_INSTANCE_ID, null)
        if (stored != null && INSTANCE_ID_REGEX.matches(stored)) stored
        else {
            val fresh = newInstanceId()
            prefs.edit().putString(KEY_INSTANCE_ID, fresh).apply()
            fresh
        }
    }

    /** 保存一个已成功配对的 token 摘要（tokenId 8 hex → sha256 64 hex）。 */
    fun saveToken(tokenId: String, tokenSha256Hex: String) {
        prefs.edit()
            .putString(tokenKey(tokenId), tokenSha256Hex.lowercase())
            .apply()
    }

    /** 查 token 摘要；未配对过返回 null。 */
    fun tokenSha256(tokenId: String): String? =
        prefs.getString(tokenKey(tokenId), null)

    /** 是否已配对过任意 PC（用于 UI 提示，不参与协议）。 */
    fun hasAnyToken(): Boolean = prefs.all?.keys?.any { it.startsWith(KEY_TOKEN_PREFIX) } == true

    private fun tokenKey(tokenId: String) = "$KEY_TOKEN_PREFIX$tokenId"

    private fun newInstanceId(): String {
        val bytes = ByteArray(4)
        random.nextBytes(bytes)
        return Digests.toHex(bytes) // 4 字节 → 8 位小写 hex
    }

    private companion object {
        const val PREFS_NAME = "screenlink_pairing"
        const val KEY_INSTANCE_ID = "instance_id"
        const val KEY_TOKEN_PREFIX = "token_sha_"
        val INSTANCE_ID_REGEX = Regex("^[0-9a-f]{8}$")
    }
}
