package fan.lancat.sticker

import android.graphics.Bitmap
import java.io.Closeable

/**
 * 一个已解析的 Lottie 动画（ThorVG，CPU 渲染）。
 *
 * - 渲染时内容按比例缩放后居中放进目标 Bitmap（与桌面 lancat-slint 的 Player 相同）；
 * - 帧号会被 clamp 到 `[0, totalFrames - 1]`；
 * - 线程安全：可以在任意线程调用，`close()` 会等正在进行的 `renderFrame()` 结束。
 *
 * 不再使用时必须 [close]，否则 native 内存（解析树 + 画布）不会释放。
 */
class LottieAnimationHandle private constructor(private var ptr: Long) : Closeable {

    /** 动画自身宽度（Lottie `w`） */
    val width: Float

    /** 动画自身高度（Lottie `h`） */
    val height: Float

    /** 总帧数（ThorVG 语义：`op - ip`） */
    val totalFrames: Float

    /** 时长（秒） */
    val duration: Float

    /** 帧率：totalFrames / duration；时长为 0 时按 30 处理（与桌面一致） */
    val fps: Float get() = if (duration > 0f) totalFrames / duration else 30f

    init {
        val wh = FloatArray(2)
        LottieNative.size(ptr, wh)
        width = wh[0]
        height = wh[1]
        totalFrames = LottieNative.totalFrames(ptr)
        duration = LottieNative.duration(ptr)
    }

    val isClosed: Boolean @Synchronized get() = ptr == 0L

    /**
     * 把第 [frame] 帧画进 [bitmap]（可变、ARGB_8888、非 HARDWARE；会先整体清成透明）。
     * 成功返回 true；句柄已关闭或 native 出错返回 false。
     */
    @Synchronized
    fun renderFrame(frame: Float, bitmap: Bitmap): Boolean {
        if (ptr == 0L) return false
        require(bitmap.config == Bitmap.Config.ARGB_8888 && bitmap.isMutable) { "need mutable ARGB_8888 bitmap" }
        return LottieNative.renderFrame(ptr, frame, bitmap) == 0
    }

    /** 新建一张 [px]×[px] 的 Bitmap 并渲染第 [frame] 帧。 */
    fun renderFrame(frame: Float, px: Int): Bitmap? {
        val bmp = Bitmap.createBitmap(px, px, Bitmap.Config.ARGB_8888)
        return if (renderFrame(frame, bmp)) bmp else null
    }

    @Synchronized
    override fun close() {
        if (ptr != 0L) {
            LottieNative.release(ptr)
            ptr = 0L
        }
    }

    companion object {
        /** 解析 Lottie JSON 字节；不是合法 Lottie 时返回 null。 */
        @JvmStatic
        fun load(json: ByteArray): LottieAnimationHandle? {
            if (json.isEmpty()) return null
            val p = LottieNative.load(json)
            return if (p == 0L) null else LottieAnimationHandle(p)
        }

        /** 编进 so 的 ThorVG 版本，例如 "1.1.0"。 */
        @JvmStatic
        val thorvgVersion: String by lazy { LottieNative.thorvgVersion() }
    }
}
