package fan.lancat.ui.screens.chat

import android.content.Context
import android.graphics.BitmapFactory
import android.net.Uri
import android.util.LruCache
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.produceState
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import fan.lancat.model.Body
import fan.lancat.model.Message
import fan.lancat.model.TransferState
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext

/**
 * 桌面 v0.2.8：收发完成的图片直接显示缩略图（app.slint kind == "image"）。
 * 这里按需从 content:// / file:// 解码一张长边 ≤ 720px 的缩略图，进程内 LRU 缓存；
 * 解码失败（文件被删、无权限）时退回普通文件卡片。
 */
object Thumbs {
    private val cache = object : LruCache<String, ImageBitmap>(48 * 1024 * 1024) {
        override fun sizeOf(key: String, value: ImageBitmap) = value.width * value.height * 4
    }
    private val failed = HashSet<String>()

    fun cached(uri: String): ImageBitmap? = cache.get(uri)
    fun hasFailed(uri: String) = synchronized(failed) { uri in failed }

    fun load(ctx: Context, uri: String, maxPx: Int = 720): ImageBitmap? {
        cache.get(uri)?.let { return it }
        if (hasFailed(uri)) return null
        val bmp = runCatching {
            val u = Uri.parse(uri)
            val bounds = BitmapFactory.Options().apply { inJustDecodeBounds = true }
            ctx.contentResolver.openInputStream(u)?.use { BitmapFactory.decodeStream(it, null, bounds) }
            if (bounds.outWidth <= 0) return@runCatching null
            var sample = 1
            while (maxOf(bounds.outWidth, bounds.outHeight) / (sample * 2) >= maxPx) sample *= 2
            val opts = BitmapFactory.Options().apply { inSampleSize = sample }
            ctx.contentResolver.openInputStream(u)?.use { BitmapFactory.decodeStream(it, null, opts) }
        }.getOrNull()
        if (bmp == null) { synchronized(failed) { failed.add(uri) }; return null }
        return bmp.asImageBitmap().also { cache.put(uri, it) }
    }
}

fun isImage(f: Body.File): Boolean =
    f.mime?.startsWith("image/") == true || fileIconName(f.name) == "image"

/** 是否该按图片显示（已完成 + 有本地 uri + 是图片 + 没解码失败过）。 */
fun showsAsImage(f: Body.File): Boolean =
    f.state is TransferState.Done && f.localUri != null && isImage(f) && !Thumbs.hasFailed(f.localUri!!)

@Composable
fun ImageMessage(m: Message, f: Body.File, actions: FileActions, joinTop: Boolean, joinBottom: Boolean, modifier: Modifier = Modifier) {
    val ctx = LocalContext.current
    val uri = f.localUri!!
    val bmp by produceState(Thumbs.cached(uri), uri) {
        if (value == null) value = withContext(Dispatchers.IO) { Thumbs.load(ctx, uri) }
    }
    val b = bmp
    if (b == null) {
        // 解码中 / 失败：先按文件卡片显示
        FileCard(m, f, actions, joinTop, joinBottom, modifier)
        return
    }
    val maxW = 250f
    val maxH = 320f
    val ratio = b.width.toFloat() / b.height.coerceAtLeast(1)
    var w = maxW; var h = w / ratio
    if (h > maxH) { h = maxH; w = h * ratio }
    if (w < 120f) w = 120f
    Box(
        modifier
            .size(w.dp, h.dp)
            .clip(RoundedCornerShape(14.dp))
            .background(Color(0x22000000))
    ) {
        Image(
            b, f.name, contentScale = ContentScale.Crop,
            modifier = Modifier.matchParentSize(),
        )
        Box(
            Modifier.align(Alignment.BottomEnd).padding(6.dp)
                .background(Color(0x73000000), RoundedCornerShape(10.dp))
                .padding(horizontal = 6.dp, vertical = 1.dp)
        ) { MetaLabel(m, Color.White, Modifier.padding(top = 0.dp)) }
    }
}

