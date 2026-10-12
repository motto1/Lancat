package app.lancat.mobile

import android.app.Activity
import android.content.Context
import android.content.Intent
import android.net.Uri
import android.provider.OpenableColumns
import android.util.Log
import androidx.activity.result.ActivityResult
import app.tauri.annotation.ActivityCallback
import app.tauri.annotation.Command
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSArray
import app.tauri.plugin.JSObject
import app.tauri.plugin.Plugin
import java.io.File
import java.io.FileOutputStream
import java.util.UUID

/** Native helpers the Rust side calls (src-tauri/src/mobile.rs): file picking and the share sheet. */
@TauriPlugin
class LancatPlugin(private val activity: Activity) : Plugin(activity) {
    @Command
    fun pickFiles(invoke: Invoke) {
        val i = Intent(Intent.ACTION_OPEN_DOCUMENT).apply {
            addCategory(Intent.CATEGORY_OPENABLE)
            type = "*/*"
            putExtra(Intent.EXTRA_ALLOW_MULTIPLE, true)
        }
        startActivityForResult(invoke, i, "picked")
    }

    @ActivityCallback
    fun picked(invoke: Invoke, result: ActivityResult) {
        val data = result.data
        if (result.resultCode != Activity.RESULT_OK || data == null) {
            invoke.resolve(answer(emptyList(), false))
            return
        }
        val uris = Shared.urisOf(data)
        Thread { invoke.resolve(answer(uris.mapNotNull { Shared.copy(activity, it) }, false)) }.start()
    }

    /** Files shared to us since the last call; `busy` while some are still being copied. */
    @Command
    fun takeShared(invoke: Invoke) {
        val (paths, busy) = Shared.take()
        invoke.resolve(answer(paths, busy))
    }

    private fun answer(paths: List<String>, busy: Boolean) = JSObject().apply {
        put("paths", JSArray(paths))
        put("busy", busy)
    }
}

/** Content URIs (picker, share sheet) copied into our cache: the core sends plain files. */
object Shared {
    private val pending = mutableListOf<String>()
    private var copying = 0

    fun urisOf(data: Intent): List<Uri> {
        val out = mutableListOf<Uri>()
        val clip = data.clipData
        if (clip != null) {
            for (k in 0 until clip.itemCount) clip.getItemAt(k).uri?.let { out.add(it) }
        }
        if (out.isEmpty()) data.data?.let { out.add(it) }
        if (out.isEmpty()) {
            @Suppress("DEPRECATION")
            (data.getParcelableExtra<Uri>(Intent.EXTRA_STREAM))?.let { out.add(it) }
            @Suppress("DEPRECATION")
            data.getParcelableArrayListExtra<Uri>(Intent.EXTRA_STREAM)?.let { out.addAll(it) }
        }
        return out.distinct()
    }

    /** An ACTION_SEND / SEND_MULTIPLE intent: copy its files in the background. */
    fun fromShareIntent(ctx: Context, intent: Intent?) {
        if (intent == null || (intent.action != Intent.ACTION_SEND && intent.action != Intent.ACTION_SEND_MULTIPLE)) return
        val uris = urisOf(intent)
        if (uris.isEmpty()) return
        synchronized(this) { copying++ }
        Thread {
            val got = uris.mapNotNull { copy(ctx, it) }
            synchronized(this) {
                pending.addAll(got)
                copying--
            }
        }.start()
        intent.action = null // handled: a configuration change must not share it again
    }

    @Synchronized
    fun take(): Pair<List<String>, Boolean> {
        val r = pending.toList()
        pending.clear()
        return Pair(r, copying > 0)
    }

    fun copy(ctx: Context, uri: Uri): String? = try {
        var name: String? = null
        ctx.contentResolver.query(uri, arrayOf(OpenableColumns.DISPLAY_NAME), null, null, null)?.use { c ->
            if (c.moveToFirst()) name = c.getString(0)
        }
        val safe = (name ?: uri.lastPathSegment ?: "file").substringAfterLast('/').ifBlank { "file" }
        val dir = File(ctx.cacheDir, "outgoing/" + UUID.randomUUID().toString())
        dir.mkdirs()
        val f = File(dir, safe)
        ctx.contentResolver.openInputStream(uri)!!.use { inp -> FileOutputStream(f).use { inp.copyTo(it, 1 shl 20) } }
        f.absolutePath
    } catch (e: Exception) {
        Log.w("lancat", "copy $uri failed: $e")
        null
    }

    /** Copies older than 3 days (their transfers are long finished or given up). */
    fun cleanup(ctx: Context) {
        Thread {
            val cut = System.currentTimeMillis() - 3L * 24 * 3600 * 1000
            File(ctx.cacheDir, "outgoing").listFiles()?.forEach { d -> if (d.lastModified() < cut) d.deleteRecursively() }
        }.start()
    }
}
