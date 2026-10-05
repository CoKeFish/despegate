package io.github.cokefish.despegate.platform

import android.app.AppOpsManager
import android.app.KeyguardManager
import android.app.usage.UsageEvents
import android.app.usage.UsageStatsManager
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.graphics.Bitmap
import android.graphics.Canvas
import android.os.PowerManager
import android.os.Process
import android.os.SystemClock
import java.io.ByteArrayOutputStream

/** What the phone can tell about how it is being used. */
class Phone(private val context: Context) {
    private val power = context.getSystemService(PowerManager::class.java)
    private val keyguard = context.getSystemService(KeyguardManager::class.java)
    private val usage = context.getSystemService(UsageStatsManager::class.java)
    private var lastActive = SystemClock.elapsedRealtime()
    private var lastQuery = System.currentTimeMillis() - 60_000
    private var front: String? = null

    /** The screen is on and past its own lock: someone is using the phone. */
    val inUse: Boolean get() = power.isInteractive && !keyguard.isKeyguardLocked

    /** Seconds since the phone was last in use. */
    fun idleSecs(): Long {
        val now = SystemClock.elapsedRealtime()
        if (inUse) lastActive = now
        return (now - lastActive) / 1000
    }

    /** Whether the system has been told not to put despegate to sleep to save battery. */
    val batteryFree: Boolean get() = power.isIgnoringBatteryOptimizations(context.packageName)

    /** Whether the user has let despegate see which app is in front. */
    val hasUsageAccess: Boolean
        get() {
            val ops = context.getSystemService(AppOpsManager::class.java)
            @Suppress("DEPRECATION")
            val mode = ops.checkOpNoThrow(AppOpsManager.OPSTR_GET_USAGE_STATS, Process.myUid(), context.packageName)
            return mode == AppOpsManager.MODE_ALLOWED
        }

    /** The package in front, as far as the usage events tell; null without usage access. */
    fun foreground(): String? {
        if (!hasUsageAccess) return null
        val now = System.currentTimeMillis()
        val events = usage.queryEvents(lastQuery, now)
        val event = UsageEvents.Event()
        while (events.hasNextEvent()) {
            events.getNextEvent(event)
            if (event.eventType == UsageEvents.Event.ACTIVITY_RESUMED) front = event.packageName
        }
        lastQuery = now
        return front
    }
}

/** The apps on the phone, by the names and icons their owner knows them by. */
class Apps(private val context: Context) {
    private val labels = HashMap<String, String>()

    /** Every app with an icon in the launcher, as package and name, sorted by name. */
    fun launchable(): List<Pair<String, String>> {
        val intent = Intent(Intent.ACTION_MAIN).addCategory(Intent.CATEGORY_LAUNCHER)
        return context.packageManager.queryIntentActivities(intent, PackageManager.MATCH_DISABLED_UNTIL_USED_COMPONENTS)
            .map { it.activityInfo.packageName }
            .distinct()
            .filter { it != context.packageName }
            .map { it to label(it) }
            .sortedBy { it.second.lowercase() }
    }

    fun label(pkg: String): String = labels.getOrPut(pkg) {
        runCatching {
            val info = context.packageManager.getApplicationInfo(pkg, 0)
            context.packageManager.getApplicationLabel(info).toString()
        }.getOrDefault(pkg)
    }

    /** The app's icon as a PNG, for the pages. */
    fun icon(pkg: String): ByteArray? = runCatching {
        val drawable = context.packageManager.getApplicationIcon(pkg)
        val bitmap = Bitmap.createBitmap(96, 96, Bitmap.Config.ARGB_8888)
        drawable.setBounds(0, 0, 96, 96)
        drawable.draw(Canvas(bitmap))
        ByteArrayOutputStream().also { bitmap.compress(Bitmap.CompressFormat.PNG, 100, it) }.toByteArray()
    }.getOrNull()

    fun launchIntent(pkg: String): Intent? = context.packageManager.getLaunchIntentForPackage(pkg)
}
