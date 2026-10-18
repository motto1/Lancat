package fan.lancat.ui.screens.chat

import android.content.ClipData
import android.content.ContentUris
import android.content.Context
import android.content.Intent
import android.net.Uri
import android.os.Environment
import android.provider.DocumentsContract
import android.provider.MediaStore
import android.util.Log
import android.webkit.MimeTypeMap
import androidx.core.content.FileProvider
import fan.lancat.model.Body
import java.io.File

/**
 * 把收发的文件交给系统：打开（关联的默认应用）和「在文件夹中显示」。
 *
 * - 本地记录的是 file:// 路径（core 直接写 Download/Lancat）或 content://。Android 7+ 不允许把 file:// 交给别的应用
 *   （FileUriExposedException 是 RuntimeException，之前没接住 → 点图片 / 文件整个应用闪退回桌面）。
 *   file:// 先换成本应用 FileProvider 的 content://（file_paths.xml 覆盖整个主存储和应用目录），换不了再查 MediaStore。
 * - 打开用普通 ACTION_VIEW（不强制弹选择器）：系统用默认应用，没有默认时弹系统自己的「打开方式」（可设为始终）。
 * - 所有异常都接住，返回 false 由调用方提示，不会再闪退。
 */
object FileIntents {
    private const val TAG = "FileIntents"
    private const val EXT_AUTH = "com.android.externalstorage.documents"

    fun mimeOf(f: Body.File): String = mimeOf(f.name, f.mime)

    fun mimeOf(name: String, mime: String?): String =
        mime?.takeIf { it.isNotBlank() && it != "*/*" }
            ?: MimeTypeMap.getSingleton().getMimeTypeFromExtension(name.substringAfterLast('.', "").lowercase())
            ?: "application/octet-stream"

    /** 本地文件（file:// 时）；content:// 返回 null */
    fun localFile(uri: String): File? {
        val u = Uri.parse(uri)
        return when (u.scheme) {
            "file" -> u.path?.let(::File)
            null, "" -> File(uri)
            else -> null
        }
    }

    /** 可以交给别的应用的 content:// uri */
    fun shareableUri(ctx: Context, uri: String): Uri? {
        val u = Uri.parse(uri)
        if (u.scheme == "content") return u
        val f = localFile(uri) ?: return null
        if (!f.exists()) return null
        runCatching { return FileProvider.getUriForFile(ctx, "${ctx.packageName}.files", f) }
            .onFailure { Log.w(TAG, "FileProvider: ${f.path}: $it") }
        return mediaStoreUri(ctx, f)
    }

    private fun mediaStoreUri(ctx: Context, f: File): Uri? = runCatching {
        val files = MediaStore.Files.getContentUri("external")
        ctx.contentResolver.query(files, arrayOf(MediaStore.MediaColumns._ID), "${MediaStore.MediaColumns.DATA}=?", arrayOf(f.absolutePath), null)?.use { c ->
            if (c.moveToFirst()) ContentUris.withAppendedId(files, c.getLong(0)) else null
        }
    }.getOrNull()

    private fun start(ctx: Context, i: Intent): Boolean = try {
        ctx.startActivity(i.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK))
        true
    } catch (e: Exception) {
        // ActivityNotFound / Security / 某些 ROM 的 IllegalArgument：都不让它闪退
        Log.w(TAG, "startActivity $i: $e")
        false
    }

    /** 用系统关联的应用打开 */
    fun open(ctx: Context, f: Body.File): Boolean = f.localUri?.let { open(ctx, it, f.name, f.mime) } ?: false

    /** 同上，按 uri（通知里的「打开」也走这里） */
    fun open(ctx: Context, src: String, name: String, mime: String?): Boolean {
        val u = shareableUri(ctx, src) ?: return false
        val type = mimeOf(name, mime)
        val i = Intent(Intent.ACTION_VIEW).setDataAndType(u, type)
            .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
        i.clipData = ClipData.newRawUri(name, u)
        if (start(ctx, i)) return true
        // 具体类型没有应用认领（如 .heic 在老设备）：退一步按大类 / 任意类型
        val major = type.substringBefore('/', "")
        for (t in listOfNotNull(major.takeIf { it.isNotEmpty() }?.let { "$it/*" }, "*/*")) {
            if (start(ctx, Intent(i).setDataAndType(u, t))) return true
        }
        return false
    }

    /** 主存储上的文件 → ExternalStorageProvider 的文档 id（primary:Download/Lancat/a.jpg） */
    private fun primaryDocId(f: File): String? {
        @Suppress("DEPRECATION")
        val root = Environment.getExternalStorageDirectory().absolutePath.trimEnd('/')
        val p = f.absolutePath
        val alt = listOf(root, "/sdcard", "/storage/self/primary")
        val base = alt.firstOrNull { p == it || p.startsWith("$it/") } ?: return null
        return "primary:" + p.removePrefix(base).trimStart('/')
    }

    private val documentsUi = listOf("com.google.android.documentsui", "com.android.documentsui")

    /**
     * 「在文件夹中显示」：打开这个文件所在的那一层文件夹（不再固定是 Download/Lancat）。
     * Android 没有「在文件管理器里选中某个文件」的公开接口；系统「文件」（DocumentsUI）收到文件本身的文档 uri 时会
     * 打开它的上一级文件夹（AOSP ActionHandler.launchToDocument），这是能做到的最精确的定位。
     * 顺序：系统「文件」打开文件所在层 → 任意文件管理器打开所在文件夹 → 系统「下载」。
     */
    fun reveal(ctx: Context, f: Body.File): Boolean {
        val ok = reveal(ctx, f.localUri)
        // 文件管理器只能打开所在文件夹、不能替我们选中文件：提示一下是哪一个
        if (ok) runCatching {
            val where = displayPath(f)?.substringBeforeLast('/', "")?.ifEmpty { null }
            android.widget.Toast.makeText(ctx, if (where != null) "$where 里的「${f.name}」" else f.name, android.widget.Toast.LENGTH_LONG).show()
        }
        return ok
    }

    fun reveal(ctx: Context, src: String?): Boolean {
        val dirMime = DocumentsContract.Document.MIME_TYPE_DIR
        val candidates = mutableListOf<Intent>()
        val file = src?.let(::localFile)
        val docId = file?.let(::primaryDocId)
        if (docId != null) {
            val fileDoc = DocumentsContract.buildDocumentUri(EXT_AUTH, docId)
            val parentDoc = DocumentsContract.buildDocumentUri(EXT_AUTH, docId.substringBeforeLast('/', "primary:"))
            for (pkg in documentsUi) {
                candidates += Intent(Intent.ACTION_VIEW).setDataAndType(fileDoc, dirMime).setPackage(pkg)
            }
            candidates += Intent(Intent.ACTION_VIEW).setDataAndType(parentDoc, dirMime)
        } else if (src != null && Uri.parse(src).scheme == "content" && runCatching { DocumentsContract.isDocumentUri(ctx, Uri.parse(src)) }.getOrDefault(false)) {
            // 用户自选的 SAF 目录里的文件（content://…/document/…）
            val doc = Uri.parse(src)
            for (pkg in documentsUi) {
                candidates += Intent(Intent.ACTION_VIEW).setDataAndType(doc, dirMime).setPackage(pkg)
                    .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
            }
        }
        candidates += Intent(android.app.DownloadManager.ACTION_VIEW_DOWNLOADS)
        return candidates.any { start(ctx, it) }
    }

    /** 给用户看的位置（Download/Lancat/a.jpg） */
    fun displayPath(f: Body.File): String? {
        val file = f.localUri?.let(::localFile) ?: return null
        return primaryDocId(file)?.removePrefix("primary:") ?: file.absolutePath
    }
}
