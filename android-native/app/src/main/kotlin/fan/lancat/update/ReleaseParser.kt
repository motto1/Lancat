package fan.lancat.update

import fan.lancat.model.AppRelease
import fan.lancat.model.AppVersion
import org.json.JSONArray
import org.json.JSONObject

/**
 * 解析 GitHub `GET /repos/motto1/Lancat/releases` 的返回，挑出带安卓安装包的最新正式发布。
 *
 * 发布约定（.github/workflows/android-native.yml 在 v* 标签上上传）：
 *  - `Lancat-android.apk`：签名的通用 APK（固定文件名，与桌面 `lancat-slint-<平台>` 单文件同一思路）
 *  - `Lancat-android.apk.sha256`：`<64 位十六进制>  Lancat-android.apk`
 * 兼容：别的 `Lancat*android*.apk` 命名；校验值也可以来自 `SHA256SUMS`（桌面格式）或 GitHub 资产自带的 `digest: "sha256:…"`。
 * 草稿、预发布、没有安卓包的发布（例如只有桌面版）都跳过。
 */
object ReleaseParser {
    const val APK = "Lancat-android.apk"
    private val APK_RE = Regex("""^lancat.*android.*\.apk$""", RegexOption.IGNORE_CASE)
    private val HEX64 = Regex("""\b[0-9a-fA-F]{64}\b""")

    /** 校验文件（.sha256 或 SHA256SUMS）的地址；由调用方下载后交给 [shaFromText] */
    data class Candidate(val release: AppRelease, val shaFileUrl: String?, val shaFileIsSums: Boolean)

    fun pickLatest(json: String): Candidate? {
        val arr = runCatching { JSONArray(json) }.getOrNull() ?: runCatching { JSONArray().put(JSONObject(json)) }.getOrNull() ?: return null
        var best: Candidate? = null
        var bestV: AppVersion.Parsed? = null
        for (i in 0 until arr.length()) {
            val c = parseRelease(arr.optJSONObject(i) ?: continue) ?: continue
            val v = AppVersion.parse(c.release.version) ?: continue
            if (bestV == null || v > bestV) { best = c; bestV = v }
        }
        return best
    }

    fun parseRelease(j: JSONObject): Candidate? {
        if (j.optBoolean("draft") || j.optBoolean("prerelease")) return null
        val tag = j.optString("tag_name").ifEmpty { return null }
        val version = tag.removePrefix("v").removePrefix("V")
        val parsed = AppVersion.parse(version) ?: return null
        if (parsed.pre != null) return null
        val assets = j.optJSONArray("assets") ?: return null
        val list = (0 until assets.length()).mapNotNull { assets.optJSONObject(it) }
        val apk = list.firstOrNull { it.optString("name") == APK } ?: list.firstOrNull { APK_RE.matches(it.optString("name")) } ?: return null
        val apkName = apk.optString("name")
        val url = apk.optString("browser_download_url").ifEmpty { return null }
        val shaAsset = list.firstOrNull { it.optString("name").equals("$apkName.sha256", ignoreCase = true) }
        val sums = list.firstOrNull { it.optString("name") == "SHA256SUMS" }
        val digest = apk.optString("digest").takeIf { it.startsWith("sha256:", ignoreCase = true) }
            ?.substringAfter(':')?.lowercase()?.takeIf { HEX64.matches(it) }
        val rel = AppRelease(
            version = version,
            notes = j.optString("body").trim(),
            page = j.optString("html_url"),
            apkName = apkName,
            apkUrl = url,
            apkSize = apk.optLong("size", -1),
            sha256 = digest,
        )
        return when {
            shaAsset != null -> Candidate(rel, shaAsset.optString("browser_download_url"), false)
            sums != null -> Candidate(rel, sums.optString("browser_download_url"), true)
            else -> Candidate(rel, null, false)
        }
    }

    /** 从 .sha256（第一段十六进制）或 SHA256SUMS（按文件名找行）里取校验值 */
    fun shaFromText(text: String, apkName: String, isSums: Boolean): String? {
        if (isSums) {
            for (line in text.lineSequence()) {
                val parts = line.trim().split(Regex("""\s+"""), limit = 2)
                if (parts.size == 2 && parts[1].trimStart('*') == apkName && HEX64.matches(parts[0])) return parts[0].lowercase()
            }
            return null
        }
        return HEX64.find(text)?.value?.lowercase()
    }
}
