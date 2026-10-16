package fan.lancat.avatar

import android.graphics.Color
import androidx.test.ext.junit.runners.AndroidJUnit4
import org.junit.Assert.assertEquals
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.annotation.Config

@RunWith(AndroidJUnit4::class)
@Config(sdk = [35])
class AvatarTest {
    // 期望值由桌面端 Rust 算法（u32 wrapping, encode_utf16）在 Python 里复算
    @Test fun hueIndexMatchesDesktop() {
        assertEquals(0, Avatar.hueIndex("abc"))
        assertEquals(4, Avatar.hueIndex("4f3c2a9e8b7d6c5e4f3a2b1c0d9e8f7a"))
        assertEquals(0, Avatar.hueIndex("LAPTOP-6GQFLSNE"))
        assertEquals(4, Avatar.hueIndex("😀x"))
        assertEquals(0, Avatar.hueIndex(""))
    }

    @Test fun colorsHue211() {
        val (top, bottom) = Avatar.colors("abc")
        assertEquals(Color.rgb(60, 138, 221), bottom)
        // 顶部 = 0.82*c + 0.18
        assertEquals(Color.rgb(95, 159, 227), top)
    }

    @Test fun initials() {
        assertEquals("?", Avatar.initials("  "))
        assertEquals("小", Avatar.initials("Motto 小米 14"))
        assertEquals("ML", Avatar.initials("motto-laptop"))
        assertEquals("MA", Avatar.initials("macbook"))
        assertEquals("LA", Avatar.initials("LAPTOP"))
        assertEquals("PX", Avatar.initials("pixel_x.y"))
    }

    @Test fun bitmapSize() {
        val b = Avatar.bitmap("abc", "MacBook", 96, adaptive = true)
        assertEquals(96, b.width)
    }
}
