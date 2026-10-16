package fan.lancat.sticker

import android.graphics.Bitmap

/**
 * liblancatsticker.so 的 JNI 入口（src/main/cpp/lc_lottie_jni.cpp）。只给 [LottieAnimationHandle] 用。
 * 所有函数线程安全：native 侧用一把进程级锁串行化 ThorVG 调用。
 */
internal object LottieNative {
    init {
        System.loadLibrary("lancatsticker")
    }

    /** 解析 Lottie JSON，失败返回 0。 */
    external fun load(json: ByteArray): Long

    /** 写入动画原始宽高到 out[0], out[1]。 */
    external fun size(handle: Long, out: FloatArray)

    external fun totalFrames(handle: Long): Float

    /** 秒 */
    external fun duration(handle: Long): Float

    /** 直接写 Bitmap 像素（必须是可变的 ARGB_8888、非 HARDWARE）。0 = 成功。 */
    external fun renderFrame(handle: Long, frame: Float, bitmap: Bitmap): Int

    external fun release(handle: Long)

    external fun thorvgVersion(): String
}
