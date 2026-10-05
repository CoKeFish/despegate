package io.github.cokefish.despegate.platform

import android.app.ActivityManager
import android.app.admin.DeviceAdminReceiver
import android.app.admin.DevicePolicyManager
import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.content.pm.PackageManager
import android.os.Build
import android.os.UserManager
import android.provider.Settings
import android.telecom.TelecomManager
import android.util.Log

/** What makes despegate a device administrator; the system talks to it, nothing else does. */
class Admin : DeviceAdminReceiver() {
    override fun onEnabled(context: Context, intent: Intent) = Guard.start(context)
}

/**
 * The control despegate has over the phone as its device owner: fixing the
 * screen to the lock page and the allowed apps, suspending apps, and closing
 * the side doors. Without ownership every call here quietly does nothing.
 */
class Device(private val context: Context) {
    private val dpm = context.getSystemService(DevicePolicyManager::class.java)
    private val admin = ComponentName(context, Admin::class.java)
    private val home = ComponentName(context.packageName, "${context.packageName}.ui.Home")
    private val pkg: String = context.packageName

    val isOwner: Boolean get() = dpm.isDeviceOwnerApp(pkg)

    /** The command that hands the phone over, for the settings page to show. */
    val ownerCommand: String get() = "adb shell dpm set-device-owner $pkg/${Admin::class.java.name.removePrefix(pkg)}"

    /** The apps a phone call goes through, which stay reachable whatever happens. */
    fun dialers(): Set<String> {
        val telecom = context.getSystemService(TelecomManager::class.java)
        val known = listOf("com.android.server.telecom", "com.android.incallui", "com.samsung.android.incallui", "com.android.phone")
        return (listOfNotNull(telecom?.defaultDialerPackage) + known.filter(::installed)).toSet()
    }

    /** Packages that must never be blocked: despegate itself, calls, and what the system stands on. */
    fun protectedApps(): Set<String> {
        val launcher = context.packageManager
            .resolveActivity(Intent(Intent.ACTION_MAIN).addCategory(Intent.CATEGORY_HOME), PackageManager.MATCH_DEFAULT_ONLY)
            ?.activityInfo?.packageName
        val keyboard = Settings.Secure.getString(context.contentResolver, Settings.Secure.DEFAULT_INPUT_METHOD)?.substringBefore('/')
        return dialers() + listOfNotNull(pkg, "android", "com.android.systemui", "com.android.settings", launcher, keyboard)
    }

    /**
     * Suspends exactly [wanted], giving back whatever of [held] is no longer
     * wanted. Returns what is suspended now, to be remembered across restarts.
     */
    fun block(wanted: Set<String>, held: Set<String>): Set<String> {
        if (!isOwner) return emptySet()
        val release = held - wanted
        val take = (wanted - held).filter(::installed)
        return guarded(held) {
            if (release.isNotEmpty()) dpm.setPackagesSuspended(admin, release.toTypedArray(), false)
            val refused = if (take.isEmpty()) emptyArray() else dpm.setPackagesSuspended(admin, take.toTypedArray(), true)
            (held - release) + (take - refused.toSet())
        }
    }

    /**
     * The packages among [candidates] that are suspended right now. Only the
     * device owner suspends apps, so these are despegate's to give back even
     * if it has lost track of them.
     */
    fun suspended(candidates: List<String>): Set<String> {
        if (!isOwner) return emptySet()
        return candidates.filter { name -> guarded(false) { dpm.isPackageSuspended(admin, name) } }.toSet()
    }

    /** Fixes the phone to the lock page, the dialer and [allowed]; Home leads back to the lock page. */
    fun lock(allowed: List<String>) {
        if (!isOwner) return
        guarded(Unit) {
            val packages = (listOf(pkg) + allowed + dialers()).distinct()
            dpm.setLockTaskPackages(admin, packages.toTypedArray())
            dpm.setLockTaskFeatures(
                admin,
                DevicePolicyManager.LOCK_TASK_FEATURE_HOME or
                    DevicePolicyManager.LOCK_TASK_FEATURE_KEYGUARD or
                    DevicePolicyManager.LOCK_TASK_FEATURE_SYSTEM_INFO or
                    DevicePolicyManager.LOCK_TASK_FEATURE_GLOBAL_ACTIONS,
            )
            context.packageManager.setComponentEnabledSetting(home, PackageManager.COMPONENT_ENABLED_STATE_ENABLED, PackageManager.DONT_KILL_APP)
            val filter = IntentFilter(Intent.ACTION_MAIN).apply {
                addCategory(Intent.CATEGORY_HOME)
                addCategory(Intent.CATEGORY_DEFAULT)
            }
            dpm.addPersistentPreferredActivity(admin, filter, home)
        }
    }

    /** Gives the screen back: no fixed apps, and Home is the launcher again. */
    fun unlock() {
        if (!isOwner) return
        guarded(Unit) {
            dpm.clearPackagePersistentPreferredActivities(admin, pkg)
            context.packageManager.setComponentEnabledSetting(home, PackageManager.COMPONENT_ENABLED_STATE_DISABLED, PackageManager.DONT_KILL_APP)
            dpm.setLockTaskPackages(admin, emptyArray())
        }
    }

    /** Whether the phone is fixed to the allowed apps right now. */
    fun isLocked(): Boolean =
        context.getSystemService(ActivityManager::class.java).lockTaskModeState != ActivityManager.LOCK_TASK_MODE_NONE

    /**
     * Closes the side doors. Safe mode and extra users would leave despegate
     * behind at any time; moving the clock only matters while [lockedIn], when
     * nothing may be loosened.
     */
    fun harden(lockedIn: Boolean, blockedNotice: String) {
        if (!isOwner) return
        guarded(Unit) {
            dpm.setUninstallBlocked(admin, pkg, true)
            // What the system says when a suspended app is tapped, instead of pointing at an IT admin.
            dpm.setOrganizationName(admin, "despegate")
            dpm.setShortSupportMessage(admin, blockedNotice)
            for (door in listOf(UserManager.DISALLOW_SAFE_BOOT, UserManager.DISALLOW_ADD_USER)) dpm.addUserRestriction(admin, door)
            if (lockedIn) dpm.addUserRestriction(admin, UserManager.DISALLOW_CONFIG_DATE_TIME)
            else dpm.clearUserRestriction(admin, UserManager.DISALLOW_CONFIG_DATE_TIME)
            if (Build.VERSION.SDK_INT >= 33) {
                dpm.setPermissionGrantState(admin, pkg, android.Manifest.permission.POST_NOTIFICATIONS, DevicePolicyManager.PERMISSION_GRANT_STATE_GRANTED)
            }
        }
    }

    /**
     * The way out: gives back everything in [held], opens every door and stops
     * being the device owner. After this the app can be uninstalled like any other.
     */
    @Suppress("DEPRECATION")
    fun release(held: Set<String>) {
        if (!isOwner) return
        block(emptySet(), held)
        unlock()
        guarded(Unit) {
            for (door in listOf(UserManager.DISALLOW_SAFE_BOOT, UserManager.DISALLOW_ADD_USER, UserManager.DISALLOW_CONFIG_DATE_TIME)) {
                dpm.clearUserRestriction(admin, door)
            }
            dpm.setUninstallBlocked(admin, pkg, false)
            dpm.clearDeviceOwnerApp(pkg)
        }
        runCatching { dpm.removeActiveAdmin(admin) }
    }

    private fun installed(name: String): Boolean =
        runCatching { context.packageManager.getApplicationInfo(name, 0) }.isSuccess

    /** The system refuses now and then (a package gone, ownership just lost); that must not take the app down. */
    private fun <T> guarded(fallback: T, action: () -> T): T =
        try {
            action()
        } catch (e: RuntimeException) {
            Log.w("despegate", "the system refused", e)
            fallback
        }
}
