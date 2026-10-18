package fan.lancat.icon

import android.content.Context
import android.graphics.Bitmap
import android.graphics.Canvas
import android.graphics.drawable.AdaptiveIconDrawable
import androidx.core.content.ContextCompat
import androidx.test.core.app.ApplicationProvider
import androidx.test.ext.junit.runners.AndroidJUnit4
import fan.lancat.R
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode
import java.io.File

/**
 * 用真实资源渲染启动图标 / 主题图标 / 通知小图标，输出到 build/icon-render/ 供人工查看，并做基本断言：
 * 自适应图标中心是猫的橙色、角落是米色底；通知图标只有白色 + 透明（单色剪影）。
 */
@RunWith(AndroidJUnit4::class)
@GraphicsMode(GraphicsMode.Mode.NATIVE)
@Config(sdk = [35], qualifiers = "xxxhdpi")
class LauncherIconRenderTest {
    private val ctx: Context = ApplicationProvider.getApplicationContext()
    private val out = File("build/icon-render").apply { mkdirs() }

    private fun render(id: Int, px: Int, name: String): Bitmap {
        val d = ContextCompat.getDrawable(ctx, id)!!
        val bmp = Bitmap.createBitmap(px, px, Bitmap.Config.ARGB_8888)
        d.setBounds(0, 0, px, px); d.draw(Canvas(bmp))
        File(out, "$name.png").outputStream().use { bmp.compress(Bitmap.CompressFormat.PNG, 100, it) }
        return bmp
    }

    @Test fun launcher() {
        val d = ContextCompat.getDrawable(ctx, R.mipmap.ic_launcher)
        assertTrue("ic_launcher 应是自适应图标", d is AdaptiveIconDrawable)
        assertTrue("应有主题图标（monochrome）层", (d as AdaptiveIconDrawable).monochrome != null)
        val b = render(R.mipmap.ic_launcher, 432, "ic_launcher")
        render(R.mipmap.ic_launcher_round, 432, "ic_launcher_round")
        render(R.drawable.ic_launcher_foreground, 432, "ic_launcher_foreground")
        render(R.drawable.ic_launcher_monochrome, 432, "ic_launcher_monochrome")
        // 中心偏下是猫身（橙红：R 高、B 低）
        val c = b.getPixel(216, 250)
        val r = (c shr 16) and 0xFF; val bl = c and 0xFF
        assertTrue("center should be cat orange, was ${Integer.toHexString(c)}", r > 180 && bl < 120)
        // 安全区外靠近边缘的是米色底（#F3EEE3）或遮罩外透明
        val e = b.getPixel(216, 40)
        assertTrue("top area should be background, was ${Integer.toHexString(e)}", (e ushr 24) == 0 || ((e shr 16) and 0xFF) > 230)
    }

    @Test fun notificationIconIsMonochrome() {
        val b = render(R.drawable.ic_notify, 96, "ic_notify")
        var opaque = 0
        for (y in 0 until b.height) for (x in 0 until b.width) {
            val p = b.getPixel(x, y); val a = p ushr 24
            if (a > 0) {
                val rgb = p and 0xFFFFFF
                assertTrue("ic_notify must be white only, got ${Integer.toHexString(p)}", rgb == 0xFFFFFF || a < 255)
                if (a == 255) opaque++
            }
        }
        assertTrue(opaque > 96 * 96 / 6)
    }
}
