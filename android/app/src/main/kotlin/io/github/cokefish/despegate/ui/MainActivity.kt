package io.github.cokefish.despegate.ui

import android.app.Activity
import android.Manifest
import android.content.Intent
import android.content.pm.PackageManager
import android.net.Uri
import android.os.Build
import android.os.Bundle
import android.provider.Settings
import io.github.cokefish.despegate.core
import io.github.cokefish.despegate.platform.Guard
import org.json.JSONArray
import org.json.JSONObject

/** The settings: a page that asks for the current state and for every change it wants made. */
class MainActivity : Activity() {
    private lateinit var page: Page
    /** The message a file picker was opened for, answered when it closes. */
    private var picking: Any? = null

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        page = Page(this, ::answer)
        page.show("index.html", "__I18N__" to core.texts.section("ui.", "day.", "uninstall.").toString())
        // An owner grants itself the right to warn; otherwise it is the user's to give.
        if (Build.VERSION.SDK_INT >= 33 && !core.device.isOwner && checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED) {
            requestPermissions(arrayOf(Manifest.permission.POST_NOTIFICATIONS), NOTICES)
        }
    }

    override fun onResume() {
        super.onResume()
        Guard.start(this)
        page.run("window.__refresh && window.__refresh()")
    }

    private fun answer(message: JSONObject): JSONObject? = when (message.optString("kind")) {
        "state" -> JSONObject().put("state", core.pageState())
        "pulse" -> JSONObject().put("pulse", core.pulse())
        "do" -> {
            val outcome = core.handle(message.optJSONObject("request") ?: JSONObject())
            if (outcome.ok) Guard.poke(this)
            JSONObject().put("ok", outcome.ok).put("text", outcome.message)
        }
        "apps" -> JSONObject().put("apps", JSONArray(core.apps.launchable().map { (pkg, label) -> JSONObject().put("pkg", pkg).put("label", label) }))
        "pick" -> {
            picking = message.opt("id")
            val intent = Intent(Intent.ACTION_OPEN_DOCUMENT)
                .addCategory(Intent.CATEGORY_OPENABLE)
                .setType("*/*")
                .putExtra(Intent.EXTRA_MIME_TYPES, arrayOf("image/*", "video/*"))
                .putExtra(Intent.EXTRA_ALLOW_MULTIPLE, true)
            @Suppress("DEPRECATION")
            startActivityForResult(intent, PICK)
            null
        }
        "theme" -> {
            page.dress(message.optString("bar"), message.optBoolean("dark"))
            null
        }
        "usage_access" -> open(Intent(Settings.ACTION_USAGE_ACCESS_SETTINGS))
        "accessibility" -> open(Intent(Settings.ACTION_ACCESSIBILITY_SETTINGS))
        "battery" -> open(Intent(Settings.ACTION_REQUEST_IGNORE_BATTERY_OPTIMIZATIONS, Uri.parse("package:$packageName")))
        "uninstall" -> uninstall(message.optString("phrase"))
        // The texts change with the language: the page is shown again.
        "reload" -> {
            recreate()
            null
        }
        else -> JSONObject().put("ok", false).put("text", core.texts.tr("error.unknown", "cmd" to message.optString("kind")))
    }

    private fun open(intent: Intent): JSONObject? {
        runCatching { startActivity(intent) }
        return null
    }

    @Deprecated("Deprecated in Java")
    override fun onActivityResult(requestCode: Int, resultCode: Int, data: Intent?) {
        @Suppress("DEPRECATION")
        super.onActivityResult(requestCode, resultCode, data)
        if (requestCode != PICK) return
        val uris = buildList {
            data?.data?.let(::add)
            data?.clipData?.let { clip -> repeat(clip.itemCount) { add(clip.getItemAt(it).uri) } }
        }.distinct()
        val results = uris.map { uri -> core.importMedia(uri).let { JSONObject().put("ok", it.ok).put("text", it.message) } }
        page.reply(picking, JSONObject().put("results", JSONArray(results)))
        picking = null
    }

    /** Back closes whatever the page has open before it leaves the app. */
    @Deprecated("Deprecated in Java")
    override fun onBackPressed() {
        page.run("!!(window.__back && window.__back())") { handled ->
            if (handled != "true") moveTaskToBack(true)
        }
    }

    private companion object {
        const val PICK = 1
        const val NOTICES = 2
    }
}
