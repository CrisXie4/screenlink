package dev.screenlink.companion.saf

import android.content.ContentResolver
import android.net.Uri
import android.provider.DocumentsContract
import java.io.IOException
import java.io.OutputStream

/**
 * B6：SAF 写入器（DESIGN.md §4 B 对外契约 / §8 流程）。
 *
 * 全部方法走 ContentResolver I/O，调用方（transfer 引擎）须在非主线程调用。
 * 错误约定：
 *  - 未授权/授权失效 → [SecurityException]（对端可见 reason=DENIED 语义）；
 *  - 建文档/开流失败 → [IOException]（对端可见 reason=IO_ERROR 语义，PROTOCOL.md §6.4）。
 *  - 删除是清理语义，永不抛出。
 */
class SafWriter(private val store: SafStore, private val resolver: ContentResolver) {

    /** 是否持有可用保存目录（含 DESIGN §8.3 的树根探活，本身含 I/O）。 */
    fun hasValidTree(): Boolean = store.current() != null

    /**
     * 新建文档并返回其 document uri。
     * 提供方撞名会自动改名（"name (1).ext"）：调用方在写历史行 fileName 前，必须用
     * [displayNameOf] 回查返回文档的真实 DISPLAY_NAME（DESIGN §8.4）。
     */
    fun createUnique(displayName: String, mime: String): Uri {
        require(displayName.isNotBlank()) { "displayName is blank" }
        require(
            !displayName.contains('/') &&
                !displayName.contains('\\') &&
                displayName != "." &&
                displayName != ".."
        ) { "invalid display name: $displayName" } // PROTOCOL.md §5.3 name 净化：协议层拒绝之外的第二道防线
        val tree = requireTree()
        val created = DocumentsContract.createDocument(
            resolver,
            tree,
            mime.ifBlank { DEFAULT_MIME },
            displayName,
        )
        return created ?: throw IOException("createDocument returned null for $displayName")
    }

    /** 全新写入（mode "w"，截断既有内容）。 */
    fun openOverwrite(uri: Uri): OutputStream = openStream(uri, MODE_OVERWRITE)

    /** 续传追加写入（mode "wa"，FileSystemProvider 支持，DESIGN §8.5）。 */
    fun openAppend(uri: Uri): OutputStream = openStream(uri, MODE_APPEND)

    /** 文档字节数；目录/提供方不报 size 或查询失败返回 [SIZE_UNKNOWN]。 */
    fun sizeOf(uri: Uri): Long = try {
        resolver.query(uri, arrayOf(DocumentsContract.Document.COLUMN_SIZE), null, null, null)
            ?.use { c ->
                if (c.moveToFirst() && !c.isNull(0)) c.getLong(0) else SIZE_UNKNOWN
            } ?: SIZE_UNKNOWN
    } catch (_: Exception) {
        SIZE_UNKNOWN
    }

    /**
     * 在授权树内按 DISPLAY_NAME 查找既有文档（断点续传，DESIGN §8.6），
     * 返回 (document uri, 已有字节数)；size 为 [SIZE_UNKNOWN] 表示提供方未报大小。
     * 跳过目录；未找到/未授权/查询失败返回 null。
     * 契约签名不带期望大小参数，故"size ≤ 期望大小"的过滤由调用方完成
     * （PROTOCOL.md §6.2.2：命中且半成品 size ≤ 请求 size 才取 offset）。
     */
    fun findByName(displayName: String): Pair<Uri, Long>? {
        val tree = store.current() ?: return null
        val childrenUri = DocumentsContract.buildChildDocumentsUriUsingTree(
            tree,
            DocumentsContract.getTreeDocumentId(tree),
        )
        val projection = arrayOf(
            DocumentsContract.Document.COLUMN_DOCUMENT_ID,
            DocumentsContract.Document.COLUMN_DISPLAY_NAME,
            DocumentsContract.Document.COLUMN_SIZE,
            DocumentsContract.Document.COLUMN_MIME_TYPE,
        )
        return try {
            resolver.query(childrenUri, projection, null, null, null)?.use { c ->
                while (c.moveToNext()) {
                    if (c.isNull(1) || c.getString(1) != displayName) continue
                    if (c.getString(3) == DocumentsContract.Document.MIME_TYPE_DIR) continue
                    val docId = c.getString(0) ?: continue
                    val size = if (c.isNull(2)) SIZE_UNKNOWN else c.getLong(2)
                    return DocumentsContract.buildDocumentUriUsingTree(tree, docId) to size
                }
                null
            }
        } catch (_: Exception) {
            null
        }
    }

    /**
     * 回查文档真实 DISPLAY_NAME（createUnique 被自动改名后写历史行用，DESIGN §8.4）；
     * 查不到返回 null。
     */
    fun displayNameOf(uri: Uri): String? = try {
        resolver.query(uri, arrayOf(DocumentsContract.Document.COLUMN_DISPLAY_NAME), null, null, null)
            ?.use { c ->
                if (c.moveToFirst() && !c.isNull(0)) c.getString(0) else null
            }
    } catch (_: Exception) {
        null
    }

    /**
     * 删除文档（失败清理：CANCEL keep=false 或 MD5_MISMATCH，PROTOCOL.md §6.4）。
     * 文档已不存在/授权失效等清理场景一律吞掉异常（清理不产生二次错误；
     * 应用删历史仅删行，不动用户目录文件，DESIGN §8.8）。
     */
    fun delete(uri: Uri) {
        try {
            DocumentsContract.deleteDocument(resolver, uri)
        } catch (_: Exception) {
            // 已删除 / SecurityException / IllegalArgumentException：清理语义下忽略
        }
    }

    private fun requireTree(): Uri =
        store.current()
            ?: throw SecurityException("SAF 保存目录未授权或已失效，请先在设置中选择保存目录")

    private fun openStream(uri: Uri, mode: String): OutputStream =
        resolver.openOutputStream(uri, mode)
            ?: throw IOException("openOutputStream(mode=$mode) returned null: $uri")

    private companion object {
        const val DEFAULT_MIME = "application/octet-stream"
        const val MODE_OVERWRITE = "w"
        const val MODE_APPEND = "wa"
        const val SIZE_UNKNOWN = -1L
    }
}
