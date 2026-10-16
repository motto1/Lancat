package fan.lancat.update

import androidx.test.ext.junit.runners.AndroidJUnit4
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.annotation.Config

/** GitHub Releases 列表解析（org.json 走 Robolectric 的真实实现） */
@RunWith(AndroidJUnit4::class)
@Config(sdk = [35])
class ReleaseParserTest {
    private fun asset(name: String, size: Long = 100, digest: String? = null) =
        """{"name":"$name","size":$size,"browser_download_url":"https://github.com/motto1/Lancat/releases/download/x/$name"${if (digest != null) ""","digest":"$digest"""" else ""}}"""

    private fun rel(tag: String, vararg assets: String, draft: Boolean = false, pre: Boolean = false) =
        """{"tag_name":"$tag","draft":$draft,"prerelease":$pre,"body":"notes $tag","html_url":"https://github.com/motto1/Lancat/releases/tag/$tag","assets":[${assets.joinToString(",")}]}"""

    @Test fun picksNewestWithAndroidApk() {
        val json = "[" + listOf(
            rel("v0.3.0", asset("lancat-slint-windows-x64.exe"), asset("SHA256SUMS")),          // 只有桌面版：跳过
            rel("v0.2.10", asset("Lancat-android.apk", 31_000_000), asset("Lancat-android.apk.sha256")),
            rel("v0.2.9", asset("Lancat-android.apk"), asset("Lancat-android.apk.sha256")),
            rel("v0.4.0", asset("Lancat-android.apk"), draft = true),
            rel("v0.5.0-beta", asset("Lancat-android.apk"), pre = true),
        ).joinToString(",") + "]"
        val c = ReleaseParser.pickLatest(json)
        assertNotNull(c)
        assertEquals("0.2.10", c!!.release.version)
        assertEquals("Lancat-android.apk", c.release.apkName)
        assertEquals(31_000_000L, c.release.apkSize)
        assertEquals("https://github.com/motto1/Lancat/releases/download/x/Lancat-android.apk.sha256", c.shaFileUrl)
        assertEquals(false, c.shaFileIsSums)
        assertEquals("notes v0.2.10", c.release.notes)
    }

    @Test fun androidRevisionTag() {
        val json = "[" + rel("v0.2.8-android.2", asset("Lancat-v0.2.8-android.2-android.apk")) + "," + rel("v0.2.8", asset("Lancat-android.apk")) + "]"
        val c = ReleaseParser.pickLatest(json)!!
        assertEquals("0.2.8-android.2", c.release.version)
        assertEquals("Lancat-v0.2.8-android.2-android.apk", c.release.apkName)
        assertNull(c.shaFileUrl)
        assertNull(c.release.sha256)
    }

    @Test fun shaSources() {
        val hex = "ab".repeat(32)
        val withDigest = ReleaseParser.pickLatest("[" + rel("v1.0.0", asset("Lancat-android.apk", digest = "sha256:$hex")) + "]")!!
        assertEquals(hex, withDigest.release.sha256)
        val withSums = ReleaseParser.pickLatest("[" + rel("v1.0.0", asset("Lancat-android.apk"), asset("SHA256SUMS")) + "]")!!
        assertEquals(true, withSums.shaFileIsSums)

        assertEquals(hex, ReleaseParser.shaFromText("${hex.uppercase()}  Lancat-android.apk\n", "Lancat-android.apk", false))
        val sums = "${"cd".repeat(32)}  lancat-slint-linux-x64\n$hex *Lancat-android.apk\n"
        assertEquals(hex, ReleaseParser.shaFromText(sums, "Lancat-android.apk", true))
        assertNull(ReleaseParser.shaFromText(sums, "other.apk", true))
        assertNull(ReleaseParser.shaFromText("garbage", "Lancat-android.apk", false))
    }

    @Test fun latestObjectAndGarbage() {
        // /releases/latest 返回单个对象也能解析
        assertEquals("1.2.3", ReleaseParser.pickLatest(rel("v1.2.3", asset("Lancat-android.apk")))!!.release.version)
        assertNull(ReleaseParser.pickLatest("not json"))
        assertNull(ReleaseParser.pickLatest("[]"))
    }
}
