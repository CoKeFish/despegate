package io.github.cokefish.despegate.ui

import android.annotation.SuppressLint
import android.app.Activity
import android.content.ActivityNotFoundException
import android.content.Intent
import android.content.res.Configuration
import android.graphics.Bitmap
import android.graphics.Color
import android.media.MediaMetadataRetriever
import android.net.Uri
import android.os.Build
import android.view.View
import android.webkit.JavascriptInterface
import android.webkit.WebResourceRequest
import android.webkit.WebResourceResponse
import android.webkit.WebView
import android.webkit.WebViewClient
import io.github.cokefish.despegate.Core
import io.github.cokefish.despegate.ORIGIN
import io.github.cokefish.despegate.core
import org.json.JSONObject
import java.io.ByteArrayInputStream
import java.io.ByteArrayOutputStream
import java.io.File
import java.io.FilterInputStream
import java.io.InputStream

/**
 * What the two screens share: each is a page inside a web view, served from
 * the app itself, that talks to the app through `window.ipc.postMessage` and
 * hears back through `window.__reply`.
 */
class Page(private val activity: Activity, private val onMessage: (JSONObject) -> JSONObject?) {
    val web = WebView(activity)
    private val core: Core = activity.core

    /** The theme chosen in the settings, or the one the phone is in when it is left to follow it. */
    private val dark: Boolean
        get() = when (core.config.appearance) {
            "dark" -> true
            "light" -> false
            else -> activity.resources.configuration.uiMode and Configuration.UI_MODE_NIGHT_MASK == Configuration.UI_MODE_NIGHT_YES
        }

    init {
        configure()
        activity.setContentView(web)
    }

    /** Shows `assets/[name]` with the texts and whatever else it needs filled in. */
    fun show(name: String, vararg fill: Pair<String, String>) {
        val t = core.texts
        var html = core.asset(name)
            .replace("__LANG__", t.code)
            .replace("__THEME__", if (dark) "dark" else "light")
            .replace("__VERSION__", activity.packageManager.getPackageInfo(activity.packageName, 0).versionName.orEmpty())
        for ((key, value) in fill) html = html.replace(key, value)
        web.loadDataWithBaseURL("$ORIGIN/", html, "text/html", "utf-8", null)
    }

    /** Hands [json] to the page as the answer to the message numbered [id]. */
    fun reply(id: Any?, json: JSONObject) {
        json.put("id", id ?: JSONObject.NULL)
        web.evaluateJavascript("window.__reply($json)", null)
    }

    fun run(script: String, result: ((String) -> Unit)? = null) = web.evaluateJavascript(script, result)

    /** Paints the system bars like the page, which says whether it is showing its dark theme. */
    fun dress(bar: String, dark: Boolean) {
        val color = runCatching { Color.parseColor(bar) }.getOrDefault(Color.BLACK)
        activity.window.statusBarColor = color
        activity.window.navigationBarColor = color
        @Suppress("DEPRECATION")
        activity.window.decorView.systemUiVisibility =
            if (dark) 0
            else View.SYSTEM_UI_FLAG_LIGHT_STATUS_BAR or (if (Build.VERSION.SDK_INT >= 26) View.SYSTEM_UI_FLAG_LIGHT_NAVIGATION_BAR else 0)
    }

    @SuppressLint("SetJavaScriptEnabled")
    private fun configure() {
        web.settings.javaScriptEnabled = true
        web.settings.domStorageEnabled = true
        web.settings.mediaPlaybackRequiresUserGesture = false
        web.settings.allowFileAccess = false
        web.setBackgroundColor(Color.TRANSPARENT)
        web.overScrollMode = View.OVER_SCROLL_NEVER
        web.isLongClickable = false
        web.setOnLongClickListener { true }
        web.addJavascriptInterface(object {
            @JavascriptInterface
            fun postMessage(raw: String) {
                activity.runOnUiThread {
                    val message = runCatching { JSONObject(raw) }.getOrNull() ?: return@runOnUiThread
                    val answer = onMessage(message)
                    if (answer != null && message.has("id")) reply(message.opt("id"), answer)
                }
            }
        }, "ipc")
        web.webViewClient = object : WebViewClient() {
            override fun shouldInterceptRequest(view: WebView, request: WebResourceRequest): WebResourceResponse? =
                if (request.url.toString().startsWith("$ORIGIN/")) serve(request) else null

            // Links to the web open in the browser; the page itself never leaves.
            override fun shouldOverrideUrlLoading(view: WebView, request: WebResourceRequest): Boolean {
                if (request.url.toString().startsWith("$ORIGIN/")) return false
                try {
                    activity.startActivity(Intent(Intent.ACTION_VIEW, request.url))
                } catch (_: ActivityNotFoundException) {
                } catch (_: SecurityException) {
                }
                return true
            }
        }
    }

    /** Fonts, app icons and the user's photos and videos, which the pages ask for by address. */
    private fun serve(request: WebResourceRequest): WebResourceResponse {
        val path = request.url.pathSegments
        return when (path.firstOrNull()) {
            "fonts" -> runCatching { found("font/woff2", activity.assets.open("fonts/${path[1]}")) }.getOrElse { missing() }
            "icon" -> core.apps.icon(path.getOrElse(1) { "" })?.let { found("image/png", ByteArrayInputStream(it)) } ?: missing()
            "media" -> media(File(core.mediaDir, path.getOrElse(1) { "" }), request.requestHeaders["Range"])
            "thumb" -> still(File(core.mediaDir, path.getOrElse(1) { "" }))
            else -> missing()
        }
    }

    /** Videos are fetched in ranges; without honouring them playback stalls. */
    private fun media(file: File, range: String?): WebResourceResponse {
        if (file.parentFile != core.mediaDir || !file.isFile) return missing()
        val total = file.length()
        val mime = when (file.extension.lowercase()) {
            "jpg", "jpeg" -> "image/jpeg"
            "png" -> "image/png"
            "gif" -> "image/gif"
            "webp" -> "image/webp"
            "mp4" -> "video/mp4"
            "webm" -> "video/webm"
            else -> "application/octet-stream"
        }
        val wanted = range?.removePrefix("bytes=")?.split('-')
        val start = wanted?.getOrNull(0)?.toLongOrNull()
        if (start == null || start >= total) {
            return WebResourceResponse(mime, null, 200, "OK", mapOf("Accept-Ranges" to "bytes", "Content-Length" to "$total"), file.inputStream())
        }
        val end = (wanted.getOrNull(1)?.toLongOrNull() ?: (total - 1)).coerceAtMost(total - 1)
        val stream = file.inputStream().also { it.skip(start) }
        val headers = mapOf("Accept-Ranges" to "bytes", "Content-Range" to "bytes $start-$end/$total", "Content-Length" to "${end - start + 1}")
        return WebResourceResponse(mime, null, 206, "Partial Content", headers, Limited(stream, end - start + 1))
    }

    /** A frame of a video, small, for the gallery: a web view shows no picture of a video until it plays. */
    private fun still(file: File): WebResourceResponse {
        if (file.parentFile != core.mediaDir || !file.isFile) return missing()
        val retriever = MediaMetadataRetriever()
        val frame = try {
            retriever.setDataSource(file.path)
            retriever.frameAtTime
        } catch (_: RuntimeException) {
            null
        } finally {
            retriever.release()
        } ?: return missing()
        val width = minOf(frame.width, 480)
        val small = Bitmap.createScaledBitmap(frame, width, frame.height * width / frame.width, true)
        val jpeg = ByteArrayOutputStream().also { small.compress(Bitmap.CompressFormat.JPEG, 80, it) }
        return found("image/jpeg", ByteArrayInputStream(jpeg.toByteArray()))
    }

    private fun found(mime: String, data: InputStream) = WebResourceResponse(mime, null, data)

    private fun missing() = WebResourceResponse("text/plain", "utf-8", 404, "Not Found", emptyMap(), ByteArrayInputStream(ByteArray(0)))

    /** A stream that ends after [left] bytes. */
    private class Limited(source: InputStream, private var left: Long) : FilterInputStream(source) {
        override fun read(): Int = if (left <= 0) -1 else super.read().also { if (it >= 0) left-- }

        override fun read(buffer: ByteArray, offset: Int, length: Int): Int {
            if (left <= 0) return -1
            val read = super.read(buffer, offset, minOf(length.toLong(), left).toInt())
            if (read > 0) left -= read
            return read
        }
    }
}

/**
 * The way out, shared by both screens: with the phrase typed, every block is
 * lifted, the phone is handed back and the system is asked to uninstall the app.
 */
fun Activity.uninstall(phrase: String): JSONObject {
    val t = core.texts
    if (phrase.trim().lowercase() != t.tr("uninstall.phrase").lowercase()) {
        return JSONObject().put("ok", false).put("text", t.tr("error.phrase"))
    }
    runCatching { stopLockTask() }
    core.release()
    startActivity(Intent(Intent.ACTION_DELETE, Uri.parse("package:$packageName")).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK))
    return JSONObject().put("ok", true).put("text", "")
}
