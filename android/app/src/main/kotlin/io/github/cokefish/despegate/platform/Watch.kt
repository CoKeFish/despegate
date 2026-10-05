package io.github.cokefish.despegate.platform

import android.accessibilityservice.AccessibilityService
import android.accessibilityservice.GestureDescription
import android.graphics.Path
import android.graphics.Rect
import android.content.Context
import android.content.Intent
import android.os.SystemClock
import android.provider.Settings
import android.view.accessibility.AccessibilityEvent
import android.view.accessibility.AccessibilityNodeInfo
import android.view.accessibility.AccessibilityWindowInfo
import android.widget.Toast
import io.github.cokefish.despegate.core
import io.github.cokefish.despegate.core.Mode

/**
 * How despegate enforces on a phone it does not own: as an accessibility
 * service it is told which app comes to the front, and answers by putting the
 * lock page back, or by sending a blocked app home. It can be switched off
 * from the phone's settings, so while a block is active or about to start it
 * also turns away the settings screens that name despegate.
 *
 * It reads a screen's contents only to look for its own name on those
 * settings screens, and keeps nothing of what it sees.
 */
class Watch : AccessibilityService() {
    private var surfaces: Set<String> = emptySet()
    private var homes: Set<String> = emptySet()
    private var surfacesAt = 0L

    override fun onServiceConnected() {
        instance = this
        core.enforcer.start()
    }

    override fun onUnbind(intent: Intent?): Boolean {
        instance = null
        front = null
        core.enforcer.wake()
        return false
    }

    override fun onInterrupt() = Unit

    override fun onAccessibilityEvent(event: AccessibilityEvent) {
        val pkg = event.packageName?.toString() ?: return
        when (event.eventType) {
            AccessibilityEvent.TYPE_WINDOW_STATE_CHANGED -> {
                // The keyboard and the system's own surfaces come and go over whatever is in front.
                if (pkg in surfaces()) return
                front = pkg
                if (!core.device.isOwner) react(pkg)
            }
            AccessibilityEvent.TYPE_WINDOW_CONTENT_CHANGED -> if (pkg == front && !core.device.isOwner) guardItself(pkg)
        }
    }

    private fun react(pkg: String) {
        val verdict = core.verdict
        val mode = verdict.mode
        when {
            mode is Mode.Lock -> if (!passes(this, pkg, mode.allowed)) core.enforcer.showLock()
            pkg in verdict.blocked -> turnAway(core.whyBlocked(pkg))
            else -> guardItself(pkg)
        }
    }

    /**
     * While nothing may be loosened, the screens that could switch despegate
     * off are closed: the settings that name it, and a launcher asking whether
     * to uninstall it.
     */
    private fun guardItself(pkg: String) {
        val launcher = pkg !in SETTINGS && pkg in launchers()
        if (pkg !in SETTINGS && !launcher) return
        val why = core.whyLockedIn() ?: return
        val window = rootInActiveWindow ?: return
        if (window.findAccessibilityNodeInfosByText(NAME).isNullOrEmpty()) return
        // A home screen shows the name under its icon all the time; only an offer to uninstall counts there.
        if (launcher && UNINSTALL.none { !window.findAccessibilityNodeInfosByText(it).isNullOrEmpty() }) return
        turnAway(why)
    }

    private fun launchers(): Set<String> {
        if (homes.isEmpty()) {
            homes = packageManager.queryIntentActivities(Intent(Intent.ACTION_MAIN).addCategory(Intent.CATEGORY_HOME), 0)
                .map { it.activityInfo.packageName }.toSet() - packageName
        }
        return homes
    }

    /**
     * A video left floating over the screen keeps in view what was meant to be
     * put away. It is closed unless its app is one that may be in front: any
     * that is not [allowed] while the screen is locked, a blocked one otherwise.
     */
    fun closeFloating(locked: Boolean, allowed: List<String>, blocked: Set<String>) {
        for (window in windows) {
            if (!window.isInPictureInPictureMode) continue
            val root = window.root ?: continue
            val owner = owner(root)
            val stays = if (locked) passes(this, owner, allowed) else owner !in blocked
            if (!stays) flickAway(window)
        }
    }

    /**
     * Drags a floating window down to where the system discards it, the way a
     * finger would: there is no other way to close one from outside its app.
     */
    private fun flickAway(window: AccessibilityWindowInfo) {
        val bounds = Rect().also(window::getBoundsInScreen)
        val screen = resources.displayMetrics
        val path = Path().apply {
            moveTo(bounds.exactCenterX(), bounds.exactCenterY())
            lineTo(screen.widthPixels / 2f, screen.heightPixels - 56 * screen.density)
        }
        dispatchGesture(GestureDescription.Builder().addStroke(GestureDescription.StrokeDescription(path, 0, 700)).build(), null, null)
    }

    /** The app a floating window belongs to: the first package under it that is not the system's own. */
    private fun owner(root: AccessibilityNodeInfo): String? {
        val queue = ArrayDeque(listOf(root))
        var looked = 0
        while (queue.isNotEmpty() && looked++ < 40) {
            val node = queue.removeFirst()
            val pkg = node.packageName?.toString()
            if (pkg != null && pkg !in surfaces()) return pkg
            for (i in 0 until node.childCount) node.getChild(i)?.let(queue::add)
        }
        return null
    }

    private fun turnAway(why: String) {
        performGlobalAction(GLOBAL_ACTION_HOME)
        Toast.makeText(this, why, Toast.LENGTH_LONG).show()
    }

    private fun surfaces(): Set<String> {
        val now = SystemClock.elapsedRealtime()
        if (surfaces.isEmpty() || now - surfacesAt > 60_000) {
            surfaces = systemSurfaces(this)
            surfacesAt = now
        }
        return surfaces
    }

    companion object {
        private const val NAME = "despegate"
        /** How a launcher words its offer to uninstall, in the languages despegate speaks. */
        private val UNINSTALL = listOf("uninstall", "desinstal")
        /** Where an app is switched off, stopped or uninstalled. */
        private val SETTINGS = setOf(
            "com.android.settings",
            "com.samsung.accessibility",
            "com.google.android.packageinstaller",
            "com.android.packageinstaller",
            "com.samsung.android.packageinstaller",
            // Samsung's device care, where apps are put to sleep.
            "com.samsung.android.lool",
        )

        /** The clock stays within reach during a lock: an alarm that rings has to be seen to be turned off. */
        private val CLOCKS = setOf("com.sec.android.app.clockpackage", "com.google.android.deskclock", "com.android.deskclock")

        /** The running service, when the user has switched it on. */
        var instance: Watch? = null
            private set
        val connected: Boolean get() = instance != null

        /** The app in front, as last told. */
        var front: String? = null
            private set

        /** The keyboard and the system's own surfaces, which are never "the app in front". */
        fun systemSurfaces(context: Context): Set<String> {
            val keyboard = Settings.Secure.getString(context.contentResolver, Settings.Secure.DEFAULT_INPUT_METHOD)?.substringBefore('/')
            return setOfNotNull(keyboard, "android", "com.android.systemui", "com.google.android.permissioncontroller", "com.android.permissioncontroller")
        }

        /** Whether [pkg] may be in front while the screen is locked around [allowed]. Not knowing what is in front does not pass. */
        fun passes(context: Context, pkg: String?, allowed: List<String>): Boolean =
            pkg != null && (pkg == context.packageName || pkg in allowed || pkg in CLOCKS || pkg in context.core.device.dialers())

        /** Switches the service off: the way out when despegate does not own the phone. */
        fun leave() {
            instance?.disableSelf()
            instance = null
            front = null
        }
    }
}
