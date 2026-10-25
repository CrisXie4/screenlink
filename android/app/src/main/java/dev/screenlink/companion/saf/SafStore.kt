package dev.screenlink.companion.saf

import android.content.Context
import android.content.Intent
import android.net.Uri
import android.provider.DocumentsContract

/**
 * B5：tree Uri 的保存/恢复/校验（DESIGN.md §8）。
 *
 * prefs 文件 `screenlink_prefs`，键 `saf_tree_uri`。
 * `takePersistableUriPermission` 使授权跨进程重启存活（allowBackup=false 已生效，
 * 授权本就不进云备份）；`current()` 三重校验：prefs 有记录 → persistedUriPermissions
 * 中存在该 uri 且读/写齐全 → 对树根做一次子文档 query 探活（用户可能在系统设置里撤销）。
 *
 * 线程约定：save/current 都走 ContentResolver binder/文件 I/O，调用方须在非主线程调用
 * （C 的 ViewModel 包 Dispatchers.IO；SafWriter.hasValidTree/findByName 间接依赖同一约定）。
 */
class SafStore(private val context: Context) {

    private val prefs = context.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)

    /**
     * 保存 tree uri：先 takePersistableUriPermission(READ|WRITE)（拿不到持久授权则抛
     * SecurityException，此时 prefs 保持原值不动），成功后写 prefs，再尽力释放旧 uri 的
     * 持久授权（失败忽略，最多少占一个持久授权槽位）。
     */
    fun save(uri: Uri) {
        val resolver = context.contentResolver
        val flags = Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_GRANT_WRITE_URI_PERMISSION
        val old = prefs.getString(KEY_TREE_URI, null)
        resolver.takePersistableUriPermission(uri, flags)
        prefs.edit().putString(KEY_TREE_URI, uri.toString()).apply()
        if (old != null && old != uri.toString()) {
            try {
                resolver.releasePersistableUriPermission(Uri.parse(old), flags)
            } catch (_: SecurityException) {
                // 旧授权已不存在（重启被系统回收/已在设置撤销）：忽略
            }
        }
    }

    /** 启动时恢复；prefs 无记录、持久授权缺失或树根探活失败均返回 null（UI 提示重新授权）。 */
    fun current(): Uri? {
        val saved = prefs.getString(KEY_TREE_URI, null) ?: return null
        val uri = Uri.parse(saved)
        val resolver = context.contentResolver
        val granted = resolver.persistedUriPermissions.any {
            it.uri == uri && it.isReadPermission && it.isWritePermission
        }
        if (!granted) return null
        return try {
            val childrenUri = DocumentsContract.buildChildDocumentsUriUsingTree(
                uri,
                DocumentsContract.getTreeDocumentId(uri),
            )
            resolver.query(
                childrenUri,
                arrayOf(DocumentsContract.Document.COLUMN_DOCUMENT_ID),
                null,
                null,
                null,
            )?.use {
                // 能成功打开游标即探活通过（空目录同样合法），无需读出行
            }
            uri
        } catch (_: Exception) {
            // SecurityException（撤销）/ IllegalArgumentException（uri 损坏）/ 提供方异常 → 视为失效
            null
        }
    }

    private companion object {
        const val PREFS_NAME = "screenlink_prefs"
        const val KEY_TREE_URI = "saf_tree_uri"
    }
}
