package fan.lancat.sticker

import android.content.Context
import android.content.SharedPreferences
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow

/**
 * 最近使用的表情（SharedPreferences 存 id 列表，逗号分隔，最新的在前）。
 * 规则与桌面相同：用一次就挪到最前、去重、最多存 [MAX_STORED] 个；面板里显示前 [EmojiCatalog.RECENT_SHOWN] 个。
 *
 * ```
 * val recent = RecentEmojis.get(context)
 * recent.ids.collectAsState()            // StateFlow<List<String>>
 * recent.add("1f44d")                    // 发送贴纸后调用
 * EmojiCatalog.recentCategory(ids)       // 变成 EmojiCategory
 * ```
 */
class RecentEmojis private constructor(private val prefs: SharedPreferences) {

    private val state = MutableStateFlow(decode(prefs.getString(KEY, null)))

    /** 最近使用的 id，最新的在前（只含目录里存在的 id） */
    val ids: StateFlow<List<String>> = state.asStateFlow()

    /** 当前最近使用的表情（最多 [EmojiCatalog.RECENT_SHOWN] 个） */
    fun items(): List<EmojiItem> = EmojiCatalog.recentCategory(state.value).items

    fun add(id: String) {
        if (!EmojiCatalog.contains(id)) return
        val next = pushed(state.value, id)
        state.value = next
        prefs.edit().putString(KEY, encode(next)).apply()
    }

    fun remove(id: String) {
        val next = state.value - id
        state.value = next
        prefs.edit().putString(KEY, encode(next)).apply()
    }

    fun clear() {
        state.value = emptyList()
        prefs.edit().remove(KEY).apply()
    }

    companion object {
        const val PREFS = "fan.lancat.sticker"
        const val KEY = "recent_emoji"
        const val MAX_STORED = 24

        @Volatile
        private var inst: RecentEmojis? = null

        fun get(context: Context): RecentEmojis =
            inst ?: synchronized(this) {
                inst ?: RecentEmojis(context.applicationContext.getSharedPreferences(PREFS, Context.MODE_PRIVATE)).also { inst = it }
            }

        /** 纯函数：把 id 挪到最前、去重、截断（JVM 单测覆盖） */
        fun pushed(list: List<String>, id: String, max: Int = MAX_STORED): List<String> =
            (listOf(id) + list.filter { it != id }).take(max)

        internal fun encode(list: List<String>): String = list.joinToString(",")

        internal fun decode(s: String?): List<String> =
            s.orEmpty().split(',').map { it.trim() }.filter { it.isNotEmpty() && EmojiCatalog.contains(it) }.distinct().take(MAX_STORED)
    }
}

/** 便捷入口：`EmojiCatalog.recent(context).add(id)` */
fun EmojiCatalog.recent(context: Context): RecentEmojis = RecentEmojis.get(context)
