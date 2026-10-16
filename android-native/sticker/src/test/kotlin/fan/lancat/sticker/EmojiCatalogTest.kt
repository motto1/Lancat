package fan.lancat.sticker

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import java.io.File

class EmojiCatalogTest {

    @Test
    fun has116EmojiInDesktopOrder() {
        assertEquals(116, EmojiCatalog.all.size)
        assertEquals("1f600", EmojiCatalog.all.first().id)
        assertEquals("1f42c", EmojiCatalog.all.last().id)
        assertEquals(EmojiCatalog.all.size, EmojiCatalog.all.map { it.id }.toSet().size)
    }

    @Test
    fun categoriesMatchDesktop() {
        assertEquals(listOf("smileys", "gestures", "hearts", "celebrate", "animals"), EmojiCatalog.categories.map { it.id })
        assertEquals(listOf("笑脸", "手势", "爱心与符号", "庆祝与物品", "动物"), EmojiCatalog.categories.map { it.title })
        assertEquals(listOf("smile", "hand", "heart", "flag", "paw"), EmojiCatalog.categories.map { it.icon })
        assertEquals(listOf(58, 14, 20, 14, 10), EmojiCatalog.categories.map { it.items.size })
        assertEquals(116, EmojiCatalog.categories.sumOf { it.items.size })
        // 每个分类里的顺序与总表一致
        assertEquals(EmojiCatalog.all, EmojiCatalog.categories.flatMap { it.items })
    }

    @Test
    fun itemFields() {
        val heart = EmojiCatalog.byId("2764_fe0f")
        assertNotNull(heart)
        assertEquals("❤️", heart!!.char)
        assertEquals(listOf("爱心", "red", "heart", "love"), heart.keywords)
        assertEquals("爱心", heart.title)
        assertEquals("hearts", EmojiCatalog.categoryOf("2764_fe0f"))
        assertNull(EmojiCatalog.byId("nope"))
        assertFalse(EmojiCatalog.contains("../../etc/passwd"))
        assertTrue(EmojiCatalog.contains("1f44d"))
    }

    @Test
    fun searchChinese() {
        assertEquals(listOf("1f44d"), EmojiCatalog.search("赞").map { it.id })
        // "笑" 命中多个笑脸，保持目录顺序
        val laugh = EmojiCatalog.search("笑").map { it.id }
        assertTrue(laugh.size > 5)
        assertEquals(EmojiCatalog.all.map { it.id }.filter { it in laugh }, laugh)
    }

    @Test
    fun searchEnglishCaseAndTrim() {
        assertEquals(listOf("1f431"), EmojiCatalog.search("  CAT ").map { it.id })
        // 词组跨空格
        assertEquals(listOf("1f44d"), EmojiCatalog.search("thumbs up").map { it.id })
        // 子串匹配（与桌面 contains 一致）：heart 命中 love/hearts/各色心等
        val hearts = EmojiCatalog.search("heart").map { it.id }
        assertTrue("2764_fe0f" in hearts && "1f60d" in hearts && "1f494" in hearts)
    }

    @Test
    fun searchByCharAndEmpty() {
        assertEquals(listOf("1f525"), EmojiCatalog.search("🔥").map { it.id })
        assertTrue(EmojiCatalog.search("").isEmpty())
        assertTrue(EmojiCatalog.search("   ").isEmpty())
        assertTrue(EmojiCatalog.search("zzzzqqq").isEmpty())
    }

    @Test
    fun recentPushDedupTruncate() {
        var l = emptyList<String>()
        l = RecentEmojis.pushed(l, "1f600")
        l = RecentEmojis.pushed(l, "1f44d")
        l = RecentEmojis.pushed(l, "1f600")
        assertEquals(listOf("1f600", "1f44d"), l)
        val many = EmojiCatalog.all.map { it.id }.fold(emptyList<String>()) { acc, id -> RecentEmojis.pushed(acc, id) }
        assertEquals(RecentEmojis.MAX_STORED, many.size)
        assertEquals(EmojiCatalog.all.last().id, many.first())
    }

    @Test
    fun recentEncodeDecode() {
        val l = listOf("1f600", "1f44d", "2764_fe0f")
        assertEquals(l, RecentEmojis.decode(RecentEmojis.encode(l)))
        // 未知 / 重复 / 空项被丢弃
        assertEquals(listOf("1f600", "1f44d"), RecentEmojis.decode("1f600,,bogus,1f44d,1f600"))
        assertTrue(RecentEmojis.decode(null).isEmpty())
    }

    @Test
    fun recentCategoryShowsAtMost21() {
        val ids = EmojiCatalog.all.map { it.id }.take(24) + "bogus"
        val c = EmojiCatalog.recentCategory(ids)
        assertEquals("recent", c.id)
        assertEquals("最近", c.title)
        assertEquals("clock2", c.icon)
        assertEquals(EmojiCatalog.RECENT_SHOWN, c.items.size)
    }

    @Test
    fun everyEmojiHasAnAsset() {
        // 单测工作目录是模块目录
        val dir = File("src/main/assets/emoji")
        if (!dir.isDirectory) return
        for (e in EmojiCatalog.all) assertTrue("missing asset ${e.id}", File(dir, "${e.id}.z").isFile)
        assertEquals(116, dir.listFiles { f -> f.name.endsWith(".z") }!!.size)
    }
}
