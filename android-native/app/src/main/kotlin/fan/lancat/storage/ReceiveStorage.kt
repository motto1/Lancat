package fan.lancat.storage

import android.app.DownloadManager
import android.content.ContentValues
import android.content.Context
import android.content.Intent
import android.media.MediaScannerConnection
import android.net.Uri
import android.os.Build
import android.os.Environment
import android.provider.DocumentsContract
import android.provider.MediaStore
import android.webkit.MimeTypeMap
import androidx.activity.result.contract.ActivityResultContracts
import androidx.core.content.FileProvider
import java.io.File
import java.io.OutputStream

/**
 * 接收目录。Settings.receiveDir 两种取值：
 * - 默认 [DEFAULT_DIR]（"Download/Lancat"）：Android 10+ 用 MediaStore.Downloads（RELATIVE_PATH=Download/Lancat/，
 *   写入期间 IS_PENDING=1），不需要任何存储权限；Android 8–9 直接写公共 Download/Lancat（WRITE_EXTERNAL_STORAGE，maxSdk 28）
 * - SAF 树 uri（content://…/tree/…）：用户在设置里自选目录，takePersistableUriPermission(读|写) 长期有效，
 *   DocumentsContract.createDocument 建文件
 *
 * core 在 :core 里，不能依赖 :app；它需要落盘时由集成组把 [create]/[commit]/[abort] 接进去（见 CONTRACT-CHANGES.md）。
 */
object ReceiveStorage {
    const val DEFAULT_DIR = "Download/Lancat"
    private const val SUB = "Lancat"

    fun isTree(dir: String) = dir.startsWith("content://")

    class Target(val uri: Uri, val out: OutputStream, internal val legacyFile: File?)

    /** 新建一个接收文件，返回 uri 和输出流。调用方写完必须 [commit]，失败 [abort]。 */
    fun create(ctx: Context, receiveDir: String, displayName: String, mime: String?): Target {
        val name = displayName.ifBlank { "file" }
        val type = mime ?: guessMime(name)
        val cr = ctx.contentResolver
        if (isTree(receiveDir)) {
            val tree = Uri.parse(receiveDir)
            val parent = DocumentsContract.buildDocumentUriUsingTree(tree, DocumentsContract.getTreeDocumentId(tree))
            val doc = DocumentsContract.createDocument(cr, parent, type, name)
                ?: error("createDocument 失败：$receiveDir")
            return Target(doc, cr.openOutputStream(doc, "w") ?: error("openOutputStream 失败"), null)
        }
        if (Build.VERSION.SDK_INT >= 29) {
            val values = ContentValues().apply {
                put(MediaStore.Downloads.DISPLAY_NAME, name)
                put(MediaStore.Downloads.MIME_TYPE, type)
                put(MediaStore.Downloads.RELATIVE_PATH, "${Environment.DIRECTORY_DOWNLOADS}/$SUB/")
                put(MediaStore.Downloads.IS_PENDING, 1)
            }
            val uri = cr.insert(MediaStore.Downloads.getContentUri(MediaStore.VOLUME_EXTERNAL_PRIMARY), values)
                ?: error("MediaStore insert 失败")
            return Target(uri, cr.openOutputStream(uri, "w") ?: error("openOutputStream 失败"), null)
        }
        @Suppress("DEPRECATION")
        val dir = File(Environment.getExternalStoragePublicDirectory(Environment.DIRECTORY_DOWNLOADS), SUB).apply { mkdirs() }
        val f = unique(dir, name)
        return Target(Uri.fromFile(f), f.outputStream(), f)
    }

    fun commit(ctx: Context, t: Target) {
        runCatching { t.out.close() }
        if (t.legacyFile != null) {
            MediaScannerConnection.scanFile(ctx, arrayOf(t.legacyFile.absolutePath), null, null)
        } else if (Build.VERSION.SDK_INT >= 29 && t.uri.authority == MediaStore.AUTHORITY) {
            ctx.contentResolver.update(t.uri, ContentValues().apply { put(MediaStore.Downloads.IS_PENDING, 0) }, null, null)
        }
    }

    fun abort(ctx: Context, t: Target) {
        runCatching { t.out.close() }
        runCatching {
            when {
                t.legacyFile != null -> t.legacyFile.delete()
                DocumentsContract.isDocumentUri(ctx, t.uri) -> DocumentsContract.deleteDocument(ctx.contentResolver, t.uri)
                else -> ctx.contentResolver.delete(t.uri, null, null)
            }
        }
    }

    /** 设置页选择目录：registerForActivityResult(pickDirContract()) { uri -> takeTree(ctx, uri) } */
    fun pickDirContract() = ActivityResultContracts.OpenDocumentTree()

    fun takeTree(ctx: Context, tree: Uri): String {
        ctx.contentResolver.takePersistableUriPermission(
            tree, Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_GRANT_WRITE_URI_PERMISSION,
        )
        return tree.toString()
    }

    /**
     * 「打开」：file:// 转成本应用 FileProvider（file_paths.xml 覆盖整个主存储），不在范围内再查 MediaStore；
     * content:// 原样；带读授权。换不出可分享的 uri 返回 null（不能把 file:// 交出去：Android 7+ 直接抛异常闪退）。
     */
    fun openIntent(ctx: Context, uri: Uri, mime: String?): Intent? {
        val u = if (uri.scheme == "file") {
            val f = File(uri.path ?: return null)
            runCatching { FileProvider.getUriForFile(ctx, "${ctx.packageName}.files", f) }.getOrNull()
                ?: mediaStoreUri(ctx, f) ?: return null
        } else uri
        val type = mime?.takeIf { it.isNotBlank() && it != "*/*" } ?: runCatching { ctx.contentResolver.getType(u) }.getOrNull() ?: guessMime(uri.lastPathSegment ?: "")
        return Intent(Intent.ACTION_VIEW).setDataAndType(u, type)
            .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_ACTIVITY_NEW_TASK)
            .also { it.clipData = android.content.ClipData.newRawUri("", u) }
    }

    private fun mediaStoreUri(ctx: Context, f: File): Uri? = runCatching {
        val files = MediaStore.Files.getContentUri("external")
        ctx.contentResolver.query(files, arrayOf(MediaStore.MediaColumns._ID), "${MediaStore.MediaColumns.DATA}=?", arrayOf(f.absolutePath), null)?.use { c ->
            if (c.moveToFirst()) android.content.ContentUris.withAppendedId(files, c.getLong(0)) else null
        }
    }.getOrNull()

    /** 主存储上的文件 → ExternalStorageProvider 文档 id（primary:Download/Lancat/a.jpg） */
    private fun primaryDocId(f: File): String? {
        @Suppress("DEPRECATION")
        val root = Environment.getExternalStorageDirectory().absolutePath.trimEnd('/')
        val p = f.absolutePath
        val base = listOf(root, "/sdcard", "/storage/self/primary").firstOrNull { p == it || p.startsWith("$it/") } ?: return null
        return "primary:" + p.removePrefix(base).trimStart('/')
    }

    /**
     * 「在文件夹中显示」候选，按顺序尝试：
     * 1. SAF 树：用 DocumentsUI 打开该目录
     * 2. 默认目录：DocumentsUI 打开 primary:Download/Lancat（AOSP / Pixel 可用，部分 OEM 文件管理器不认）
     * 3. 系统「下载」界面 DownloadManager.ACTION_VIEW_DOWNLOADS
     */
    fun revealIntents(ctx: Context, receiveDir: String, fileUri: Uri?): List<Intent> {
        val list = mutableListOf<Intent>()
        val dirMime = DocumentsContract.Document.MIME_TYPE_DIR
        // 文件本身在主存储上：系统「文件」收到文件的文档 uri 会打开它所在的那一层（AOSP launchToDocument），再退到所在文件夹
        val docId = fileUri?.takeIf { it.scheme == "file" }?.path?.let { primaryDocId(File(it)) }
        if (docId != null) {
            val fileDoc = DocumentsContract.buildDocumentUri("com.android.externalstorage.documents", docId)
            for (pkg in listOf("com.google.android.documentsui", "com.android.documentsui")) {
                list += Intent(Intent.ACTION_VIEW).setDataAndType(fileDoc, dirMime).setPackage(pkg).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
            }
            val parent = DocumentsContract.buildDocumentUri("com.android.externalstorage.documents", docId.substringBeforeLast('/', "primary:"))
            list += Intent(Intent.ACTION_VIEW).setDataAndType(parent, dirMime).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
        } else if (fileUri != null && fileUri.scheme == "content" && runCatching { DocumentsContract.isDocumentUri(ctx, fileUri) }.getOrDefault(false)) {
            for (pkg in listOf("com.google.android.documentsui", "com.android.documentsui")) {
                list += Intent(Intent.ACTION_VIEW).setDataAndType(fileUri, dirMime).setPackage(pkg)
                    .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_ACTIVITY_NEW_TASK)
            }
        }
        if (isTree(receiveDir)) {
            val tree = Uri.parse(receiveDir)
            val doc = DocumentsContract.buildDocumentUriUsingTree(tree, DocumentsContract.getTreeDocumentId(tree))
            list += Intent(Intent.ACTION_VIEW).setDataAndType(doc, dirMime)
                .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_ACTIVITY_NEW_TASK)
        } else {
            val doc = DocumentsContract.buildDocumentUri("com.android.externalstorage.documents", "primary:${Environment.DIRECTORY_DOWNLOADS}/$SUB")
            list += Intent(Intent.ACTION_VIEW).setDataAndType(doc, dirMime)
                .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_ACTIVITY_NEW_TASK)
        }
        list += Intent(DownloadManager.ACTION_VIEW_DOWNLOADS).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
        return list
    }

    fun guessMime(name: String): String =
        MimeTypeMap.getSingleton().getMimeTypeFromExtension(name.substringAfterLast('.', "").lowercase()) ?: "application/octet-stream"

    private fun unique(dir: File, name: String): File {
        var f = File(dir, name)
        if (!f.exists()) return f
        val base = name.substringBeforeLast('.', name)
        val ext = name.substringAfterLast('.', "").let { if (it.isEmpty() || it == name) "" else ".$it" }
        var i = 1
        while (f.exists()) f = File(dir, "$base ($i)$ext").also { i++ }
        return f
    }
}
