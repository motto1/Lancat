package fan.lancat.notify

import org.junit.Assert.assertEquals
import org.junit.Test

class FormatTest {
    @Test fun bytes() {
        assertEquals("512 B", Format.bytes(512))
        assertEquals("1.0 KB", Format.bytes(1024))
        assertEquals("15 MB", Format.bytes(15L * 1024 * 1024))
        assertEquals("1.5 GB", Format.bytes(1536L * 1024 * 1024))
    }

    @Test fun eta() {
        assertEquals("剩余 5 秒", Format.eta(5))
        assertEquals("剩余 2 分 3 秒", Format.eta(123))
        assertEquals("剩余 1 小时 1 分", Format.eta(3660))
    }

    @Test fun percent() {
        assertEquals("25%", Format.percent(1, 4))
        assertEquals("…", Format.percent(1, 0))
        assertEquals(1000, Format.permille(5, 5))
    }
}
