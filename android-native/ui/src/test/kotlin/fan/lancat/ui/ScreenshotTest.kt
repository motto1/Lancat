package fan.lancat.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.remember
import androidx.compose.ui.Modifier
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onRoot
import androidx.test.ext.junit.runners.AndroidJUnit4
import com.github.takahirom.roborazzi.ExperimentalRoborazziApi
import com.github.takahirom.roborazzi.captureRoboImage
import com.github.takahirom.roborazzi.captureScreenRoboImage
import fan.lancat.ui.components.Flash
import fan.lancat.ui.components.FlashHost
import fan.lancat.ui.components.LocalFlash
import fan.lancat.ui.fake.FakeLancatRepository
import fan.lancat.ui.screens.PeerListScreen
import fan.lancat.ui.screens.SettingsScreen
import fan.lancat.ui.screens.chat.ChatDebug
import fan.lancat.ui.screens.chat.ChatScreen
import fan.lancat.ui.theme.AvatarColors
import fan.lancat.ui.theme.LancatTheme
import fan.lancat.ui.util.Clock
import fan.lancat.ui.util.LocalClock
import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import androidx.compose.ui.test.getBoundsInRoot
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.assertIsDisplayed
import fan.lancat.model.UpdateState
import fan.lancat.ui.fake.FakeAppUpdater
import fan.lancat.ui.update.LocalUpdater
import fan.lancat.ui.update.UpdateCard
import org.junit.Before
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode
import java.util.TimeZone

/** 2026-10-16 15:30 Asia/Shanghai，所有截图用同一个「现在」。 */
const val NOW = 1792135800000L

@RunWith(AndroidJUnit4::class)
@GraphicsMode(GraphicsMode.Mode.NATIVE)
@Config(sdk = [35], qualifiers = "w400dp-h860dp-xhdpi")
@OptIn(ExperimentalRoborazziApi::class)
class ScreenshotTest {
    @get:Rule val compose = createComposeRule()

    @Before fun tz() { TimeZone.setDefault(TimeZone.getTimeZone("Asia/Shanghai")) }

    private fun repo(empty: Boolean = false) = FakeLancatRepository(now = NOW, live = false, empty = empty, imageUri = sampleImage())

    /** 生成一张 4:3 的渐变「照片」放进 cache，给图片消息显示缩略图。 */
    private fun sampleImage(): String {
        val ctx = androidx.test.core.app.ApplicationProvider.getApplicationContext<android.content.Context>()
        val f = java.io.File(ctx.cacheDir, "sample.png")
        if (!f.exists()) {
            val w = 480; val h = 360
            val bmp = android.graphics.Bitmap.createBitmap(w, h, android.graphics.Bitmap.Config.ARGB_8888)
            val c = android.graphics.Canvas(bmp)
            val p = android.graphics.Paint()
            p.shader = android.graphics.LinearGradient(0f, 0f, 0f, h.toFloat(), 0xFF5AA9F7.toInt(), 0xFFF7C59F.toInt(), android.graphics.Shader.TileMode.CLAMP)
            c.drawRect(0f, 0f, w.toFloat(), h.toFloat(), p)
            p.shader = null; p.color = 0xFFFFE08A.toInt(); c.drawCircle(360f, 110f, 46f, p)
            p.color = 0xFF3D5A80.toInt()
            val path = android.graphics.Path().apply { moveTo(0f, 300f); lineTo(140f, 170f); lineTo(260f, 280f); lineTo(350f, 210f); lineTo(480f, 320f); lineTo(480f, 360f); lineTo(0f, 360f); close() }
            c.drawPath(path, p)
            f.outputStream().use { bmp.compress(android.graphics.Bitmap.CompressFormat.PNG, 100, it) }
        }
        return android.net.Uri.fromFile(f).toString()
    }

    @Composable
    private fun Frame(dark: Boolean, content: @Composable () -> Unit) {
        val flash = remember { Flash() }
        LancatTheme(dark = dark) {
            CompositionLocalProvider(LocalClock provides Clock { NOW }, LocalFlash provides flash) {
                Box(Modifier.fillMaxSize().background(LancatTheme.colors.background)) {
                    content()
                    FlashHost(flash)
                }
            }
        }
    }

    private fun shot(name: String, screen: Boolean = false, dark: Boolean, content: @Composable () -> Unit) {
        compose.setContent { Frame(dark) { content() } }
        compose.mainClock.advanceTimeBy(1500)
        compose.waitForIdle()
        val path = "screenshots/ui/${name}_${if (dark) "dark" else "light"}.png"
        if (screen) captureScreenRoboImage(path) else compose.onRoot().captureRoboImage(path)
    }

    // ---------------------------------------------------------------- 会话列表
    @Test fun peers_light() = shot("01_peers", dark = false) { PeerListScreen(repo(), {}, {}) }
    @Test fun peers_dark() = shot("01_peers", dark = true) { PeerListScreen(repo(), {}, {}) }
    @Test fun empty_light() = shot("02_empty", dark = false) { PeerListScreen(repo(empty = true), {}, {}) }
    @Test fun empty_dark() = shot("02_empty", dark = true) { PeerListScreen(repo(empty = true), {}, {}) }

    // ---------------------------------------------------------------- 聊天
    @Test fun chat_light() = shot("03_chat", dark = false) { ChatScreen(repo(), "pc-7f21", {}) }
    @Test fun chat_dark() = shot("03_chat", dark = true) { ChatScreen(repo(), "pc-7f21", {}) }
    @Test fun chatTyping_light() = shot("04_chat_text", dark = false) { ChatScreen(repo(), "arch-03b9", {}, ChatDebug(typing = true)) }
    @Test fun chatTyping_dark() = shot("04_chat_text", dark = true) { ChatScreen(repo(), "arch-03b9", {}, ChatDebug(typing = true)) }
    @Test fun emoji_light() = shot("05_emoji", dark = false) { ChatScreen(repo(), "mac-a810", {}, ChatDebug(emojiOpen = true)) }
    @Test fun emoji_dark() = shot("05_emoji", dark = true) { ChatScreen(repo(), "mac-a810", {}, ChatDebug(emojiOpen = true)) }
    @Test fun menu_light() = shot("06_menu", dark = false) { ChatScreen(repo(), "pc-7f21", {}, ChatDebug(menuOnMessageIndexFromEnd = 1)) }
    @Test fun menu_dark() = shot("06_menu", dark = true) { ChatScreen(repo(), "pc-7f21", {}, ChatDebug(menuOnMessageIndexFromEnd = 1)) }
    @Test fun select_light() = shot("07_select", dark = false) { ChatScreen(repo(), "pc-7f21", {}, ChatDebug(selectIndicesFromEnd = listOf(0, 1))) }
    @Test fun select_dark() = shot("07_select", dark = true) { ChatScreen(repo(), "pc-7f21", {}, ChatDebug(selectIndicesFromEnd = listOf(0, 1))) }
    @Test fun all_light() = shot("08_all", dark = false) { ChatScreen(repo(), "*", {}) }
    @Test fun all_dark() = shot("08_all", dark = true) { ChatScreen(repo(), "*", {}) }
    @Test fun attach_light() = shot("09_attach", screen = true, dark = false) { ChatScreen(repo(), "pc-7f21", {}, ChatDebug(attachOpen = true)) }
    @Test fun attach_dark() = shot("09_attach", screen = true, dark = true) { ChatScreen(repo(), "pc-7f21", {}, ChatDebug(attachOpen = true)) }

    // ---------------------------------------------------------------- 设置 / 分享
    @Config(qualifiers = "w400dp-h1700dp-xhdpi")
    @Test fun settings_light() = shot("10_settings", dark = false) { SettingsScreen(repo(), {}) }
    @Config(qualifiers = "w400dp-h1700dp-xhdpi")
    @Test fun settings_dark() = shot("10_settings", dark = true) { SettingsScreen(repo(), {}) }
    @Test fun share_light() = shot("11_share", dark = false) { ShareSheetContent(repo(), listOf("content://x/1", "content://x/2"), {}) }
    @Test fun share_dark() = shot("11_share", dark = true) { ShareSheetContent(repo(), listOf("content://x/1", "content://x/2"), {}) }

    // ---------------------------------------------------------------- 整体入口
    @Test fun app_light() = shot("12_app", dark = false) { LancatApp(repo()) }

    // ---------------------------------------------------------------- 少量消息从顶部排、空会话、应用内更新
    @Test fun chatOne_light() {
        shot("13_chat_one", dark = false) { ChatScreen(repo(), "pix-55c2", {}) }
        // 只有一条消息：应贴在标题栏下方，而不是沉在输入框上方
        val list = compose.onNodeWithTag("chat-list").getBoundsInRoot()
        val msg = compose.onNodeWithText("测试").getBoundsInRoot()
        assertTrue("message top ${msg.top} should be near list top ${list.top}", msg.top - list.top < (list.bottom - list.top) / 4)
    }
    @Test fun chatOne_dark() = shot("13_chat_one", dark = true) { ChatScreen(repo(), "pix-55c2", {}) }
    @Test fun chatEmpty_light() {
        shot("14_chat_empty", dark = false) { ChatScreen(repo(), "ipad-9e0d", {}) }
        compose.onNodeWithText("还没有消息").assertExists()
    }
    @Test fun chatEmpty_dark() = shot("14_chat_empty", dark = true) { ChatScreen(repo(), "ipad-9e0d", {}) }
    @Test fun updateDialog_light() = shot("15_update", dark = false) {
        Box(Modifier.fillMaxSize(), contentAlignment = androidx.compose.ui.Alignment.Center) {
            UpdateCard(UpdateState.Available(FakeAppUpdater.SAMPLE), FakeAppUpdater.SAMPLE, "0.2.8-android.1", null)
        }
    }
    @Test fun updateDialog_dark() = shot("15_update", dark = true) {
        Box(Modifier.fillMaxSize(), contentAlignment = androidx.compose.ui.Alignment.Center) {
            val r = FakeAppUpdater.SAMPLE
            UpdateCard(UpdateState.Downloading(r, 12_582_912, r.apkSize), r, "0.2.8-android.1", null)
        }
    }
    @Config(qualifiers = "w400dp-h1900dp-xhdpi")
    @Test fun settingsUpdate_light() = shot("16_settings_update", dark = false) {
        CompositionLocalProvider(LocalUpdater provides FakeAppUpdater(UpdateState.Available(FakeAppUpdater.SAMPLE))) { SettingsScreen(repo(), {}) }
    }
}

/** 头像算法与桌面 api.ts 一致性（JS 结果手算核对）。 */
class AvatarColorsTest {
    @Test fun hashMatchesJs() {
        // JS: for c of "sl-b": h = (h*31 + code) >>> 0  → 3531246
        assertEquals(3531246L, AvatarColors.hash("sl-b"))
        assertEquals(AvatarColors.hue("sl-b"), floatArrayOf(211f, 262f, 330f, 14f, 32f, 145f, 175f, 195f, 290f)[(3531246 % 9)])
    }
    @Test fun initials() {
        assertEquals("SB", AvatarColors.initials("sl-b"))
        assertEquals("MP", AvatarColors.initials("MOTTO-PC"))
        assertEquals("小", AvatarColors.initials("小米 14"))
        assertEquals("MA", AvatarColors.initials("motto-arch"))
        assertEquals("MA", AvatarColors.initials("MacBook Air"))
        assertEquals("?", AvatarColors.initials("  "))
    }
}

/** 假仓库基本行为。 */
class FakeRepoTest {
    @Test fun sendAndBlock() = runBlocking {
        val r = FakeLancatRepository(now = NOW, live = false)
        val before = r.messages("mac-a810").value.size
        r.sendText(listOf("mac-a810"), "hi")
        assertEquals(before + 1, r.messages("mac-a810").value.size)
        r.setBlocked("mac-a810", true)
        assertEquals(true, r.peers.value.first { it.id == "mac-a810" }.blocked)
        r.clearHistory(null)
        assertEquals(0, r.messages("*").value.size)
    }
}
