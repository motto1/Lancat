package fan.lancat.sticker

/**
 * 表情分类。[id] 与桌面相同："smileys" | "gestures" | "hearts" | "celebrate" | "animals"，
 * 外加动态的 "recent"（见 [RecentEmojis]）。[icon] 是桌面端的图标名（clock2/smile/hand/heart/flag/paw/sticker）。
 */
data class EmojiCategory(
    val id: String,
    val title: String,
    val items: List<EmojiItem>,
    val icon: String = "",
)

/**
 * 一个 Noto 动态表情。[id] 就是消息里 `Body.Sticker.emojiId`（码点小写十六进制，多码点用 `_` 连接，例如 "2764_fe0f"），
 * [char] 是对应的系统 emoji 字符，[keywords] 是中英文关键词（第一个是中文名，桌面端拿它做 tooltip）。
 */
data class EmojiItem(
    val id: String,
    val keywords: List<String>,
    val char: String,
) {
    /** 中文名（关键词第一个），用于无障碍描述 / tooltip。 */
    val title: String get() = keywords.firstOrNull().orEmpty()
}

/**
 * 内置的 116 个 Noto Animated Emoji（Google，CC BY 4.0），与桌面 lancat-slint 同一套、同一顺序。
 * 纯 Kotlin，不依赖 Android，可以在 JVM 单测里直接用。
 */
object EmojiCatalog {
    const val RECENT = "recent"

    /** 桌面面板里"最近使用"一栏最多显示的个数 */
    const val RECENT_SHOWN = 21

    private val kwText = HashMap<String, String>()
    private val catOf = HashMap<String, String>()

    /** 全部表情，目录顺序 */
    val all: List<EmojiItem>

    /** 静态分类（不含 "recent" 和 "stickers"），顺序与桌面相同 */
    val categories: List<EmojiCategory>

    private val byId: Map<String, EmojiItem>

    init {
        val items = ArrayList<EmojiItem>(EmojiCatalogData.EMOJI.size)
        val byCat = LinkedHashMap<String, MutableList<EmojiItem>>()
        for (row in EmojiCatalogData.EMOJI) {
            val (id, ch, cat, kw) = row
            val item = EmojiItem(id, kw.split(' ').filter { it.isNotEmpty() }, ch)
            items += item
            kwText[id] = kw.lowercase()
            catOf[id] = cat
            byCat.getOrPut(cat) { ArrayList() } += item
        }
        all = items
        byId = items.associateBy { it.id }
        categories = EmojiCatalogData.CATEGORIES
            .filter { (id) -> byCat.containsKey(id) }
            .map { (id, title, icon) -> EmojiCategory(id, title, byCat.getValue(id), icon) }
    }

    /** "最近" 分类的标题 / 图标（与桌面一致） */
    val recentTitle: String = EmojiCatalogData.CATEGORIES.first { it[0] == RECENT }[1]
    val recentIcon: String = EmojiCatalogData.CATEGORIES.first { it[0] == RECENT }[2]

    /** 按 id 查；不在目录里（例如远端发来的未知 id）返回 null */
    fun byId(id: String): EmojiItem? = byId[id]

    /** id 是否是内置表情（渲染前用它校验远端传来的 id，防止拼出任意 asset 路径） */
    fun contains(id: String): Boolean = byId.containsKey(id)

    /** 表情所属分类 id */
    fun categoryOf(id: String): String? = catOf[id]

    /**
     * 搜索，规则与桌面相同：去掉首尾空白、转小写后，关键词串（空格连接，所以 "thumbs up" 这种词组也能搜到）
     * 包含它，或者等于 emoji 字符本身。空查询返回空列表。结果保持目录顺序。
     */
    fun search(q: String): List<EmojiItem> {
        val k = q.trim().lowercase()
        if (k.isEmpty()) return emptyList()
        return all.filter { kwText.getValue(it.id).contains(k) || it.char == k }
    }

    /** 把最近使用的 id 列表变成分类（过滤掉目录里没有的 id，最多 [RECENT_SHOWN] 个） */
    fun recentCategory(recentIds: List<String>): EmojiCategory =
        EmojiCategory(RECENT, recentTitle, recentIds.mapNotNull { byId[it] }.take(RECENT_SHOWN), recentIcon)
}
