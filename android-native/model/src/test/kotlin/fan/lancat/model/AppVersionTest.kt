package fan.lancat.model

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class AppVersionTest {
    @Test fun basicOrdering() {
        assertTrue(AppVersion.isNewer("v0.2.9", "0.2.8"))
        assertTrue(AppVersion.isNewer("0.3.0", "0.2.99"))
        assertTrue(AppVersion.isNewer("1.0", "0.9.9"))
        assertTrue(AppVersion.isNewer("0.2.10", "0.2.9"))      // 数字比较，不是字符串
        assertFalse(AppVersion.isNewer("0.2.8", "0.2.8"))
        assertFalse(AppVersion.isNewer("v0.2.7", "0.2.8"))
    }

    @Test fun androidRevision() {
        assertTrue(AppVersion.isNewer("0.2.8-android.1", "0.2.8"))
        assertTrue(AppVersion.isNewer("0.2.8-android.2", "0.2.8-android.1"))
        assertTrue(AppVersion.isNewer("0.2.8-android.10", "0.2.8-android.9"))
        assertFalse(AppVersion.isNewer("0.2.8", "0.2.8-android.1"))  // 桌面同号发布不会让安卓修订版「降级」
        assertTrue(AppVersion.isNewer("0.2.9", "0.2.8-android.3"))
        assertFalse(AppVersion.isNewer("v0.2.8-android.1", "0.2.8-android.1"))
    }

    @Test fun prerelease() {
        assertFalse(AppVersion.isNewer("0.2.9-beta", "0.2.9"))
        assertTrue(AppVersion.isNewer("0.2.9-beta", "0.2.8"))
        assertTrue(AppVersion.isNewer("0.2.9", "0.2.9-rc.1"))
        assertTrue(AppVersion.isNewer("0.2.9+build.7", "0.2.8"))
    }

    @Test fun garbage() {
        assertNull(AppVersion.parse(""))
        assertNull(AppVersion.parse("latest"))
        assertNull(AppVersion.parse("1.2.3.4"))
        assertFalse(AppVersion.isNewer("nightly", "0.2.8"))
        assertTrue(AppVersion.isNewer("0.2.8", "dev"))
    }

    @Test fun versionCodes() {
        assertEquals(20800, AppVersion.versionCode("0.2.8"))
        assertEquals(20801, AppVersion.versionCode("0.2.8-android.1"))
        assertEquals(20900, AppVersion.versionCode("v0.2.9"))
        assertEquals(1000000, AppVersion.versionCode("1.0.0"))
        assertNull(AppVersion.versionCode("0.3.0-beta"))
        assertTrue(AppVersion.versionCode("0.2.8-android.1")!! > 208)   // 旧版 0.2.8 = 208，必须能覆盖安装
    }
}
