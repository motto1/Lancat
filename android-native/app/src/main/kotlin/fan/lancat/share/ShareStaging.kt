package fan.lancat.share

import android.content.ContentResolver
import android.content.Context
import android.content.Intent
import android.net.Uri
import android.provider.OpenableColumns
import android.util.Log
import androidx.core.content.FileProvider
import java.io.File
import java.util.UUID

/**
 * 分享进来的 content:// 怎样才能稳定地发出去（核实结论，见 STATUS-c.md「分享 uri 生命周期」）：
 *
 * 1. 分享方给的是临时授权，挂在接收它的 Activity 上；ShareActivity 结束后授权随之失效。
 * 2. 把 uri 放进启动 Service 的 Intent（ClipData + FLAG_GRANT_READ_URI_PERMISSION），授权就转给这个 Service：
 *    「Access will remain until the Service has called stopSelf(int) for that start command or a later one,
 *    or until the Service has been completely stopped」（frameworks/base Service.java 注释）。
 *    所以 LancatService 在所有传输结束之前不 stopSelf。
 * 3. 只有带 FLAG_GRANT_PERSISTABLE_URI_PERMISSION 的（SAF 文档 uri）能 takePersistableUriPermission，跨重启有效；
 *    普通分享基本没有这个标志，只能尽力一试。
 * 4. 进程被杀或服务停了，授权就没了，「重试」会失败。所以小文件（单个 ≤ 32 MiB，一批合计 ≤ 256 MiB）
 *    在服务里先复制到 cache/share/<uuid>/原文件名，换成本应用 FileProvider 的 uri 再交给 core；大文件不复制
 *    （复制几 GB 太慢、占空间），靠第 2 条撑到传完。
 * 5. 一些分享方（如部分浏览器、聊天应用）的 provider 在其自身进程回收后就失效，小文件复制也顺带解决这个问题。
 */
object ShareStaging {
    private const val TAG = "lancat-share"
    const val COPY_LIMIT_SINGLE = 32L * 1024 * 1024
    const val COPY_LIMIT_BATCH = 256L * 1024 * 1024
    private const val KEEP_MS = 3L * 24 * 3600 * 1000

    fun dir(ctx: Context) = File(ctx.cacheDir, "share")

    data class Staged(val uri: Uri, val copied: Boolean)

    /** 在拿着授权的 Service 里调用（IO 线程） */
    fun stage(ctx: Context, uris: List<Uri>): List<Staged> {
        val cr = ctx.contentResolver
        var budget = COPY_LIMIT_BATCH
        return uris.map { u ->
            tryPersist(cr, u)
            val size = querySize(cr, u)
            if (size in 0..COPY_LIMIT_SINGLE && size <= budget) {
                val copied = copy(ctx, u)
                if (copied != null) {
                    budget -= size
                    return@map Staged(copied, true)
                }
            }
            Staged(u, false)
        }
    }

    private fun tryPersist(cr: ContentResolver, u: Uri) {
        try {
            cr.takePersistableUriPermission(u, Intent.FLAG_GRANT_READ_URI_PERMISSION)
        } catch (_: SecurityException) {
            // 分享方没给 persistable 授权：正常情况，靠 Service 持有的临时授权
        } catch (_: Exception) {
        }
    }

    fun querySize(cr: ContentResolver, u: Uri): Long = try {
        cr.query(u, arrayOf(OpenableColumns.SIZE), null, null, null)?.use { c ->
            if (c.moveToFirst() && !c.isNull(0)) c.getLong(0) else -1L
        } ?: -1L
    } catch (e: Exception) {
        -1L
    }

    fun queryName(cr: ContentResolver, u: Uri): String? = try {
        cr.query(u, arrayOf(OpenableColumns.DISPLAY_NAME), null, null, null)?.use { c ->
            if (c.moveToFirst() && !c.isNull(0)) c.getString(0) else null
        }
    } catch (e: Exception) {
        null
    }

    private fun copy(ctx: Context, u: Uri): Uri? {
        val cr = ctx.contentResolver
        val name = sanitize(queryName(cr, u) ?: u.lastPathSegment ?: "file")
        val d = File(dir(ctx), UUID.randomUUID().toString()).apply { mkdirs() }
        val out = File(d, name)
        return try {
            cr.openInputStream(u)?.use { input -> out.outputStream().use { input.copyTo(it, 256 * 1024) } } ?: return null
            FileProvider.getUriForFile(ctx, "${ctx.packageName}.files", out)
        } catch (e: Exception) {
            Log.w(TAG, "copy $u: $e")
            d.deleteRecursively()
            null
        }
    }

    fun sanitize(name: String): String {
        val n = name.replace(Regex("[\\\\/:*?\"<>|\\u0000-\\u001f]"), "_").trim().trimStart('.')
        return n.ifEmpty { "file" }.take(180)
    }

    /** 清理 3 天前的复制件 */
    fun prune(ctx: Context, now: Long = System.currentTimeMillis()) {
        dir(ctx).listFiles()?.forEach { if (now - it.lastModified() > KEEP_MS) it.deleteRecursively() }
    }
}
