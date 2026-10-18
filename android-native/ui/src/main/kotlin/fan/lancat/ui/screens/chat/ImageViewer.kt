package fan.lancat.ui.screens.chat

import android.content.Context
import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.graphics.ImageDecoder
import android.net.Uri
import android.os.Build
import androidx.activity.compose.BackHandler
import androidx.compose.animation.core.Animatable
import androidx.compose.animation.core.tween
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.gestures.awaitEachGesture
import androidx.compose.foundation.gestures.awaitFirstDown
import androidx.compose.foundation.gestures.calculateCentroid
import androidx.compose.foundation.gestures.calculatePan
import androidx.compose.foundation.gestures.calculateZoom
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableFloatStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.produceState
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.isUnspecified
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.input.pointer.positionChanged
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.layout.onSizeChanged
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.IntSize
import androidx.compose.ui.unit.dp
import fan.lancat.model.Body
import fan.lancat.ui.components.LancatIcons
import fan.lancat.ui.components.LcIconButton
import fan.lancat.ui.theme.LancatTheme
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import kotlin.math.abs
import kotlin.math.max

/** 全屏看图用的解码：长边不超过 [maxPx]（屏幕长边的 2 倍，放大看也够清晰），按 EXIF 方向转正（Android 9+ ImageDecoder） */
private fun decodeFull(ctx: Context, uri: String, maxPx: Int): ImageBitmap? = runCatching {
    val u = Uri.parse(uri)
    val bmp: Bitmap? = if (Build.VERSION.SDK_INT >= 28) {
        val src = ImageDecoder.createSource(ctx.contentResolver, u)
        ImageDecoder.decodeBitmap(src) { dec, info, _ ->
            val (w, h) = info.size.width to info.size.height
            val long = max(w, h)
            if (long > maxPx) {
                val k = maxPx.toFloat() / long
                dec.setTargetSize((w * k).toInt().coerceAtLeast(1), (h * k).toInt().coerceAtLeast(1))
            }
            dec.allocator = ImageDecoder.ALLOCATOR_SOFTWARE
        }
    } else {
        val bounds = BitmapFactory.Options().apply { inJustDecodeBounds = true }
        ctx.contentResolver.openInputStream(u)?.use { BitmapFactory.decodeStream(it, null, bounds) }
        var sample = 1
        while (max(bounds.outWidth, bounds.outHeight) / (sample * 2) >= maxPx) sample *= 2
        ctx.contentResolver.openInputStream(u)?.use { BitmapFactory.decodeStream(it, null, BitmapFactory.Options().apply { inSampleSize = sample }) }
    }
    bmp?.asImageBitmap()
}.getOrNull()

/**
 * 全屏看图：双指缩放 / 拖动、双击放大或复原、单击显示或隐藏工具栏、未放大时下滑关闭、返回键关闭。
 * 工具栏：关闭、用其他应用打开（系统关联应用）、在文件夹中显示。
 */
@Composable
fun ImageViewer(f: Body.File, onDismiss: () -> Unit, onOpenWith: () -> Unit, onReveal: () -> Unit) {
    val ctx = LocalContext.current
    val uri = f.localUri ?: return onDismiss()
    BackHandler(onBack = onDismiss)
    val maxPx = remember {
        val dm = ctx.resources.displayMetrics
        (max(dm.widthPixels, dm.heightPixels) * 2).coerceIn(1080, 4096)
    }
    val thumb = remember(uri) { Thumbs.cached(uri) }
    val full by produceState<ImageBitmap?>(null, uri) { value = withContext(Dispatchers.IO) { decodeFull(ctx, uri, maxPx) } }
    val img = full ?: thumb

    var scale by remember { mutableFloatStateOf(1f) }
    var offset by remember { mutableStateOf(Offset.Zero) }
    var box by remember { mutableStateOf(IntSize.Zero) }
    var chrome by remember { mutableStateOf(true) }
    val dismissY = remember { Animatable(0f) }
    val appear = remember { Animatable(0f) }
    val scope = rememberCoroutineScope()
    androidx.compose.runtime.LaunchedEffect(Unit) { appear.animateTo(1f, tween(180)) }

    // 放大后限制拖动范围：图片边不离开屏幕边
    fun clamp(o: Offset, s: Float): Offset {
        val b = img ?: return Offset.Zero
        if (o.x.isNaN() || o.y.isNaN()) return Offset.Zero
        if (box.width == 0 || box.height == 0) return o
        val fit = minOf(box.width / b.width.toFloat(), box.height / b.height.toFloat())
        val w = b.width * fit * s
        val h = b.height * fit * s
        val mx = max(0f, (w - box.width) / 2f)
        val my = max(0f, (h - box.height) / 2f)
        return Offset(o.x.coerceIn(-mx, mx), o.y.coerceIn(-my, my))
    }

    val dragFrac = (abs(dismissY.value) / (box.height.coerceAtLeast(1) * 0.5f)).coerceIn(0f, 1f)
    Box(
        Modifier.fillMaxSize()
            .graphicsLayer { alpha = appear.value }
            .background(Color.Black.copy(alpha = 1f - 0.6f * dragFrac))
            .onSizeChanged { box = it }
            .pointerInput(img) {
                detectTapGestures(
                    onTap = { chrome = !chrome },
                    onDoubleTap = { p ->
                        if (scale > 1.05f) { scale = 1f; offset = Offset.Zero }
                        else {
                            val s = 2.5f
                            val c = Offset(box.width / 2f, box.height / 2f)
                            scale = s
                            offset = clamp((c - p) * (s - 1f), s)
                        }
                    },
                )
            }
            .pointerInput(img) {
                awaitEachGesture {
                    awaitFirstDown(requireUnconsumed = false)
                    var vertical: Boolean? = null
                    do {
                        val ev = awaitPointerEvent()
                        val zoom = ev.calculateZoom()
                        val pan = ev.calculatePan()
                        val pressed = ev.changes.count { it.pressed }
                        val multi = pressed > 1
                        // 抬手那一帧没有按下的指针：中心点是 Unspecified（NaN），算出来的位移会让图片整个消失
                        val cen = if (pressed > 0) ev.calculateCentroid() else Offset.Unspecified
                        if (cen.isUnspecified) continue
                        if (multi || scale > 1.01f) {
                            val ns = (scale * zoom).coerceIn(1f, 6f)
                            val c = Offset(box.width / 2f, box.height / 2f)
                            // 以手指中心为锚点缩放
                            val o = (offset + pan) * (ns / scale) + (cen - c) * (1f - ns / scale)
                            scale = ns
                            offset = if (ns <= 1.01f) Offset.Zero else clamp(o, ns)
                            ev.changes.forEach { if (it.positionChanged()) it.consume() }
                        } else if (ev.changes.size == 1) {
                            // 未放大：上下拖动关闭
                            if (vertical == null && (abs(pan.x) > 2f || abs(pan.y) > 2f)) vertical = abs(pan.y) > abs(pan.x)
                            if (vertical == true) {
                                scope.launch { dismissY.snapTo(dismissY.value + pan.y) }
                                ev.changes.forEach { if (it.positionChanged()) it.consume() }
                            }
                        }
                    } while (ev.changes.any { it.pressed })
                    if (vertical == true) {
                        if (abs(dismissY.value) > box.height * 0.18f) onDismiss()
                        else scope.launch { dismissY.animateTo(0f, tween(160)) }
                    }
                }
            },
        contentAlignment = Alignment.Center,
    ) {
        if (img != null) {
            Image(
                img, f.name, contentScale = ContentScale.Fit,
                modifier = Modifier.fillMaxSize().graphicsLayer {
                    scaleX = scale * (1f - 0.15f * dragFrac); scaleY = scaleX
                    translationX = offset.x
                    translationY = offset.y + dismissY.value
                },
            )
        }
        if (chrome) {
            Column(Modifier.fillMaxSize().safeDrawingPadding()) {
                Row(
                    Modifier.fillMaxWidth().background(Color.Black.copy(alpha = 0.35f)).padding(horizontal = 4.dp, vertical = 2.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    LcIconButton(LancatIcons.get("x"), "关闭", onDismiss, tint = Color.White)
                    Text(
                        f.name, color = Color.White, style = LancatTheme.type.body, maxLines = 1, overflow = TextOverflow.Ellipsis,
                        modifier = Modifier.weight(1f).padding(horizontal = 6.dp),
                    )
                    LcIconButton(LancatIcons.get("reveal"), "在文件夹中显示", onReveal, tint = Color.White)
                    LcIconButton(LancatIcons.get("external"), "用其他应用打开", onOpenWith, tint = Color.White)
                }
                Spacer(Modifier.weight(1f))
            }
        }
    }
}
