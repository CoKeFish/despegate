package io.github.cokefish.despegate.ui

import android.app.Activity
import android.content.ActivityNotFoundException
import android.content.Intent
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import io.github.cokefish.despegate.core
import io.github.cokefish.despegate.core.Mode
import io.github.cokefish.despegate.platform.Guard
import org.json.JSONObject
import java.lang.ref.WeakReference

/**
 * The lock screen. While it is up the phone is fixed to it and to the allowed
 * apps; Home leads back here. It leaves by itself when the block ends.
 */
class LockActivity : Activity() {
    private lateinit var page: Page
    private val handler = Handler(Looper.getMainLooper())
    private val watch = object : Runnable {
        override fun run() {
            follow()
            handler.postDelayed(this, 1000)
        }
    }
    /** What the page was last filled in with, to show it again only when it changes. */
    private var shown = ""

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        current = WeakReference(this)
        page = Page(this, ::answer)
    }

    override fun onResume() {
        super.onResume()
        visible = true
        handler.removeCallbacks(watch)
        handler.post(watch)
    }

    override fun onPause() {
        visible = false
        handler.removeCallbacks(watch)
        super.onPause()
    }

    /** Keeps the page and the fixing of the phone in step with what the guard decided. */
    private fun follow() {
        val lock = core.verdict.mode as? Mode.Lock
        if (lock == null) {
            leave()
            return
        }
        val view = core.lockView(lock).toString()
        if (view != shown) {
            shown = view
            page.show("lock.html", "__I18N__" to core.texts.section("lock.", "uninstall.").toString(), "__VIEW__" to view)
        }
        // Only an owner can have the system hold the screen; otherwise the watch brings it back.
        if (core.device.isOwner && !core.device.isLocked()) runCatching { startLockTask() }
    }

    private fun leave() {
        runCatching { stopLockTask() }
        finish()
    }

    private fun answer(message: JSONObject): JSONObject? {
        when (message.optString("kind")) {
            // Typing the emergency text exactly earns the pause; the guard then takes the lock down.
            "typed" -> if (core.typed(message.optString("typed"))) Guard.poke(this)
            "open" -> core.apps.launchIntent(message.optString("pkg"))?.let(::open)
            "call" -> open(Intent(Intent.ACTION_DIAL))
            "theme" -> page.dress(message.optString("bar"), message.optBoolean("dark"))
            "uninstall" -> return uninstall(message.optString("phrase")).also { if (it.optBoolean("ok")) finish() }
        }
        return null
    }

    private fun open(intent: Intent) {
        try {
            startActivity(intent.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK))
        } catch (_: ActivityNotFoundException) {
        } catch (_: SecurityException) {
        }
    }

    /** There is nowhere to go back to. */
    @Deprecated("Deprecated in Java")
    override fun onBackPressed() = Unit

    companion object {
        private var current = WeakReference<LockActivity>(null)

        /** Whether the lock screen is what the screen shows right now. */
        @Volatile
        var visible = false
            private set

        /** Takes the lock screen down, wherever it is. */
        fun dismiss() {
            current.get()?.let { it.runOnUiThread(it::leave) }
        }
    }
}
