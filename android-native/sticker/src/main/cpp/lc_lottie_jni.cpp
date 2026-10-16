// JNI 薄层：fan.lancat.sticker.LottieNative
//   load(ByteArray): Long            解析 Lottie JSON，失败返回 0
//   size(Long, FloatArray(2))         写入动画原始宽高
//   totalFrames(Long): Float
//   duration(Long): Float             秒
//   renderFrame(Long, Float, Bitmap): Int   直接写 AndroidBitmap 像素（ARGB_8888，预乘），0 = 成功
//   release(Long)
//   thorvgVersion(): String

#include <jni.h>
#include <android/bitmap.h>
#include <android/log.h>

#include "lc_lottie.h"

#define LOG_TAG "LancatSticker"
#define LOGW(...) __android_log_print(ANDROID_LOG_WARN, LOG_TAG, __VA_ARGS__)

static inline LcLottie* H(jlong h) { return reinterpret_cast<LcLottie*>(static_cast<intptr_t>(h)); }

extern "C" {

JNIEXPORT jlong JNICALL
Java_fan_lancat_sticker_LottieNative_load(JNIEnv* env, jobject, jbyteArray json)
{
    if (!json) return 0;
    jsize len = env->GetArrayLength(json);
    if (len <= 0) return 0;
    // GetByteArrayElements 可能复制；load 内部本来就复制一份（copy=true），用完立刻释放
    jbyte* bytes = env->GetByteArrayElements(json, nullptr);
    if (!bytes) return 0;
    LcLottie* a = lc_lottie_load(reinterpret_cast<const char*>(bytes), static_cast<uint32_t>(len));
    env->ReleaseByteArrayElements(json, bytes, JNI_ABORT);
    return static_cast<jlong>(reinterpret_cast<intptr_t>(a));
}

JNIEXPORT void JNICALL
Java_fan_lancat_sticker_LottieNative_size(JNIEnv* env, jobject, jlong h, jfloatArray out)
{
    if (!out || env->GetArrayLength(out) < 2) return;
    float wh[2] = {0, 0};
    lc_lottie_size(H(h), &wh[0], &wh[1]);
    env->SetFloatArrayRegion(out, 0, 2, wh);
}

JNIEXPORT jfloat JNICALL
Java_fan_lancat_sticker_LottieNative_totalFrames(JNIEnv*, jobject, jlong h)
{
    return lc_lottie_total_frames(H(h));
}

JNIEXPORT jfloat JNICALL
Java_fan_lancat_sticker_LottieNative_duration(JNIEnv*, jobject, jlong h)
{
    return lc_lottie_duration(H(h));
}

JNIEXPORT jint JNICALL
Java_fan_lancat_sticker_LottieNative_renderFrame(JNIEnv* env, jobject, jlong h, jfloat frame, jobject bitmap)
{
    if (!h || !bitmap) return -1;
    AndroidBitmapInfo info;
    if (AndroidBitmap_getInfo(env, bitmap, &info) != ANDROID_BITMAP_RESULT_SUCCESS) return -2;
    if (info.format != ANDROID_BITMAP_FORMAT_RGBA_8888) {
        LOGW("renderFrame: bitmap format %d, need RGBA_8888", info.format);
        return -3;
    }
    if (info.flags & ANDROID_BITMAP_FLAGS_IS_HARDWARE) return -4;
    void* px = nullptr;
    if (AndroidBitmap_lockPixels(env, bitmap, &px) != ANDROID_BITMAP_RESULT_SUCCESS || !px) return -5;
    int r = lc_lottie_render(H(h), frame, static_cast<uint32_t*>(px), info.stride / 4, info.width, info.height);
    AndroidBitmap_unlockPixels(env, bitmap);
    return r;
}

JNIEXPORT void JNICALL
Java_fan_lancat_sticker_LottieNative_release(JNIEnv*, jobject, jlong h)
{
    lc_lottie_free(H(h));
}

JNIEXPORT jstring JNICALL
Java_fan_lancat_sticker_LottieNative_thorvgVersion(JNIEnv* env, jobject)
{
    return env->NewStringUTF(lc_lottie_thorvg_version());
}

}  // extern "C"
