package fan.lancat.sticker

import android.content.Context
import android.graphics.Bitmap
import android.util.Log
import android.util.LruCache
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Deferred
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.async
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import java.io.File
import java.nio.ByteBuffer
import java.util.zip.Inflater
import java.util.zip.InflaterInputStream

/**
 * 贴纸渲染引擎（进程级单例）。思路参照 Telegram 的 RLottie 方案（自己实现，未使用其 GPL 代码）：
 *
 * - **后台渲染**：ThorVG 调用全部在 [renderDispatcher]（Dispatchers.Default 上限 [StickerConfig.renderThreads] 条）里，
 *   UI 线程只拿现成的 Bitmap；native 侧还有一把进程级锁，保证 ThorVG 的共享状态安全。
 * - **首帧缓存**：每个 (表情, 像素) 的第 0 帧做内存 LRU（[StickerConfig.posterCacheBytes]）+ 磁盘缓存
 *   （cacheDir/lancat-sticker/posters/<thorvg 版本>/，原始预乘 RGBA），聊天列表和表情面板静止时只显示它。
 * - **帧缓存**：播放中渲染出的帧按 (表情, 像素, 帧号) 放进按字节计的 LRU（[StickerConfig.frameCacheBytes]），
 *   同一个表情多处出现 / 重播时直接复用；并预取下一帧。
 * - **动画句柄**：同一表情的 [LottieAnimationHandle] 引用计数共享，最后一个使用者离开立即释放 native 内存；
 *   解压后的 JSON 另有小 LRU（[StickerConfig.jsonCacheBytes]），重新进入屏幕时只需重新解析。
 */
object StickerEngine {
    private const val TAG = "LancatSticker"

    @Volatile
    private var appContext: Context? = null

    /** 可选：在 Application.onCreate 里调用；不调用时 StickerImage 首次组合时用 LocalContext 自动初始化。 */
    fun init(context: Context) {
        if (appContext == null) appContext = context.applicationContext
    }

    /**
     * liblancatsticker.so 能否加载（JVM 单测 / Robolectric、或某些 ABI 缺库时为 false）。
     * false 时 StickerImage 退回到系统 emoji 字形，保证界面照常可用。
     */
    val nativeOk: Boolean by lazy {
        runCatching { LottieNative.thorvgVersion(); true }.getOrElse {
            Log.w(TAG, "ThorVG native unavailable, falling back to system emoji glyphs: $it")
            false
        }
    }

    internal val context: Context get() = checkNotNull(appContext) { "StickerEngine.init(context) not called" }

    @OptIn(ExperimentalCoroutinesApi::class)
    internal val renderDispatcher = Dispatchers.Default.limitedParallelism(StickerConfig.renderThreads)

    @OptIn(ExperimentalCoroutinesApi::class)
    private val ioDispatcher = Dispatchers.IO.limitedParallelism(2)

    internal val scope = CoroutineScope(SupervisorJob() + renderDispatcher)

    // ------------------------------------------------------------------ JSON

    private val jsonCache = object : LruCache<String, ByteArray>(StickerConfig.jsonCacheBytes) {
        override fun sizeOf(key: String, value: ByteArray) = value.size
    }

    /** 解压 assets/emoji/<id>.z（raw deflate，与桌面 build.rs 相同格式）。未知 id 返回 null。 */
    fun loadJson(emojiId: String): ByteArray? {
        if (!EmojiCatalog.contains(emojiId)) return null
        jsonCache.get(emojiId)?.let { return it }
        val bytes = try {
            context.assets.open("emoji/$emojiId.z").use { input ->
                InflaterInputStream(input, Inflater(true), 16 * 1024).use { it.readBytes() }
            }
        } catch (e: Exception) {
            Log.w(TAG, "load $emojiId failed", e)
            return null
        }
        jsonCache.put(emojiId, bytes)
        return bytes
    }

    // ------------------------------------------------------------------ handles

    private class Shared(val handle: LottieAnimationHandle) {
        var refs = 0
    }

    private val handles = HashMap<String, Shared>()

    /** 取得（必要时解析）共享句柄；必须配对调用 [releaseHandle]。在后台线程调用。 */
    internal fun acquireHandle(emojiId: String): LottieAnimationHandle? {
        synchronized(handles) {
            handles[emojiId]?.let { it.refs++; return it.handle }
        }
        val json = loadJson(emojiId) ?: return null
        val h = LottieAnimationHandle.load(json) ?: run {
            Log.w(TAG, "parse $emojiId failed")
            return null
        }
        synchronized(handles) {
            val existing = handles[emojiId]
            if (existing != null) { // 另一个线程抢先解析了
                existing.refs++
                h.close()
                return existing.handle
            }
            handles[emojiId] = Shared(h).also { it.refs = 1 }
            return h
        }
    }

    internal fun releaseHandle(emojiId: String) {
        val toClose = synchronized(handles) {
            val s = handles[emojiId] ?: return
            if (--s.refs > 0) return
            handles.remove(emojiId)
            s.handle
        }
        // close 会等正在进行的 renderFrame；放到后台避免卡 UI
        scope.launch { toClose.close() }
    }

    /** 当前持有的 native 句柄数（调试 / 测试用） */
    val liveHandles: Int get() = synchronized(handles) { handles.size }

    // ------------------------------------------------------------------ posters (frame 0)

    private val posters = object : LruCache<String, Bitmap>(StickerConfig.posterCacheBytes) {
        override fun sizeOf(key: String, value: Bitmap) = value.allocationByteCount
    }

    private val posterJobs = HashMap<String, Deferred<Bitmap?>>()

    private fun key(id: String, px: Int) = "$id@$px"

    /** 内存里已有的首帧（UI 线程可直接调用，用来避免首帧闪一下空白） */
    fun cachedPoster(emojiId: String, px: Int): Bitmap? = posters.get(key(emojiId, px))

    private fun posterFile(id: String, px: Int): File =
        File(context.cacheDir, "lancat-sticker/posters/${LottieAnimationHandle.thorvgVersion}/${id}@$px.rgba")

    /** 首帧：内存 → 磁盘 → 渲染。同一 key 的并发请求合并。 */
    suspend fun poster(emojiId: String, px: Int): Bitmap? {
        if (!EmojiCatalog.contains(emojiId)) return null
        val k = key(emojiId, px)
        posters.get(k)?.let { return it }
        val job = synchronized(posterJobs) {
            posterJobs[k] ?: scope.async { loadOrRenderPoster(emojiId, px) }.also { posterJobs[k] = it }
        }
        return try {
            job.await()
        } finally {
            synchronized(posterJobs) { if (posterJobs[k] === job && job.isCompleted) posterJobs.remove(k) }
        }
    }

    private suspend fun loadOrRenderPoster(id: String, px: Int): Bitmap? {
        val k = key(id, px)
        val f = posterFile(id, px)
        val want = px * px * 4
        val fromDisk = withContext(ioDispatcher) {
            runCatching {
                if (f.length() == want.toLong()) {
                    val bmp = Bitmap.createBitmap(px, px, Bitmap.Config.ARGB_8888)
                    bmp.copyPixelsFromBuffer(ByteBuffer.wrap(f.readBytes()))
                    bmp
                } else null
            }.getOrNull()
        }
        if (fromDisk != null) {
            posters.put(k, fromDisk)
            return fromDisk
        }
        val h = acquireHandle(id) ?: return null
        val bmp = try {
            h.renderFrame(0f, px)
        } finally {
            releaseHandle(id)
        } ?: return null
        posters.put(k, bmp)
        frames.put(frameKey(id, px, 0), bmp)
        withContext(ioDispatcher) {
            runCatching {
                f.parentFile?.mkdirs()
                val buf = ByteBuffer.allocate(want)
                bmp.copyPixelsToBuffer(buf)
                val tmp = File(f.path + ".tmp")
                tmp.writeBytes(buf.array())
                tmp.renameTo(f)
            }
        }
        return bmp
    }

    // ------------------------------------------------------------------ frames

    private val frames = object : LruCache<String, Bitmap>(StickerConfig.frameCacheBytes) {
        override fun sizeOf(key: String, value: Bitmap) = value.allocationByteCount
    }

    private fun frameKey(id: String, px: Int, frame: Int) = "$id@$px#$frame"

    internal fun cachedFrame(id: String, px: Int, frame: Int): Bitmap? =
        if (frame == 0) posters.get(key(id, px)) ?: frames.get(frameKey(id, px, 0)) else frames.get(frameKey(id, px, frame))

    private val rendering = java.util.concurrent.ConcurrentHashMap.newKeySet<String>()

    /**
     * 渲染一帧并放进帧缓存（在 [renderDispatcher] 上调用）。
     * 同一帧已经有别的线程在渲染时直接返回 null（调用方下一帧再来取缓存）。
     */
    internal fun renderFrame(handle: LottieAnimationHandle, id: String, px: Int, frame: Int): Bitmap? {
        cachedFrame(id, px, frame)?.let { return it }
        val fk = frameKey(id, px, frame)
        if (!rendering.add(fk)) return null
        try {
            val bmp = handle.renderFrame(frame.toFloat(), px) ?: return null
            frames.put(fk, bmp)
            if (frame == 0) posters.put(key(id, px), bmp)
            return bmp
        } finally {
            rendering.remove(fk)
        }
    }

    /** 清空内存缓存（onTrimMemory 时可调用） */
    fun trimMemory(all: Boolean = true) {
        frames.evictAll()
        if (all) {
            posters.evictAll()
            jsonCache.evictAll()
        }
    }

    /** 当前缓存占用（字节）：首帧 / 播放帧 / JSON */
    val cacheStats: Triple<Int, Int, Int> get() = Triple(posters.size(), frames.size(), jsonCache.size())
}

/** 可调参数。需要在第一次使用 [StickerEngine] / StickerImage 之前设置才对缓存大小生效。 */
object StickerConfig {
    /** 播放帧率上限（桌面端是 33 ms 一帧 ≈ 30 fps）。Noto 表情原生 60 fps，设 60 会更顺但耗电翻倍。 */
    @Volatile
    var maxFps: Int = 30

    /** 同时播放的贴纸上限（桌面 MAX_PLAYING = 6；新的挤掉最早开始的） */
    @Volatile
    var maxPlaying: Int = 6

    /** 渲染线程数。ThorVG 调用在 native 里是串行的，多一条线程用于解压 / 解析 / 分配与渲染重叠。 */
    var renderThreads: Int = 2

    /** 首帧内存缓存（字节） */
    var posterCacheBytes: Int = 12 * 1024 * 1024

    /** 播放帧内存缓存（字节）：默认 min(48 MB, 最大堆 / 8) */
    var frameCacheBytes: Int = minOf(48L * 1024 * 1024, Runtime.getRuntime().maxMemory() / 8).toInt()

    /** 解压后的 Lottie JSON 缓存（字节） */
    var jsonCacheBytes: Int = 2 * 1024 * 1024

    /** 单个贴纸最大渲染边长（px） */
    var maxPx: Int = 512
}
