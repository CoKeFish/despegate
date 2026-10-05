package io.github.cokefish.despegate.platform

import android.app.ActivityOptions
import android.app.AlarmManager
import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.os.Handler
import android.os.IBinder
import android.os.Looper
import android.util.Log
import io.github.cokefish.despegate.Core
import io.github.cokefish.despegate.Power
import io.github.cokefish.despegate.R
import io.github.cokefish.despegate.core
import io.github.cokefish.despegate.core.Mode
import io.github.cokefish.despegate.ui.LockActivity
import io.github.cokefish.despegate.ui.MainActivity
import java.time.LocalDateTime
import java.time.ZoneId

/**
 * Makes the phone match what the rules say. It looks at the clock every couple
 * of seconds while the screen is on; with the screen off there is nothing to
 * enforce, so it sleeps until the screen comes back. Whichever service is
 * alive keeps it going: the guard when despegate owns the phone, the watch
 * when it leans on accessibility.
 */
class Enforcer(private val context: Context, private val core: Core) {
    private val handler = Handler(Looper.getMainLooper())
    private val round = object : Runnable {
        override fun run() {
            tick()
            if (running) handler.postDelayed(this, TICK_MS)
        }
    }
    private val screen = object : BroadcastReceiver() {
        override fun onReceive(context: Context, intent: Intent) {
            if (intent.action == Intent.ACTION_SCREEN_OFF) {
                handler.removeCallbacks(round)
                wakeAtNextRule()
            } else {
                wake()
            }
        }
    }
    private val notices: NotificationManager get() = context.getSystemService(NotificationManager::class.java)
    private var running = false
    /** The apps the screen is currently locked around; null when it is not locked. */
    private var lockedWith: List<String>? = null
    private var hardened: Boolean? = null
    private var status = ""
    private var warning: String? = null
    /** Whether the screen is locked for a break, to know when one ends. */
    private var onBreak = false

    /** Starts looking at the clock; when already started, looks right now. */
    fun start() {
        if (!running) {
            running = true
            val t = core.texts
            notices.createNotificationChannel(NotificationChannel(STATUS, t.tr("notice.channel_status"), NotificationManager.IMPORTANCE_LOW))
            notices.createNotificationChannel(NotificationChannel(WARNINGS, t.tr("notice.channel"), NotificationManager.IMPORTANCE_HIGH))
            context.registerReceiver(screen, IntentFilter().apply {
                addAction(Intent.ACTION_SCREEN_ON)
                addAction(Intent.ACTION_SCREEN_OFF)
                addAction(Intent.ACTION_USER_PRESENT)
            })
        }
        wake()
    }

    /** Looks at the clock now instead of at the next round. */
    fun wake() {
        if (!running) return
        handler.removeCallbacks(round)
        handler.post(round)
    }

    /** What the guard shows for as long as it runs. */
    fun statusNotice(): Notification = notice(STATUS, status.substringBefore('\n').ifEmpty { "despegate" }, status.substringAfter('\n', ""))

    private fun tick() {
        val power = core.power
        if (power != Power.OWNER) Guard.stop()
        if (power == Power.NONE) {
            // Nothing left to lean on (the way out, or the watch switched off): everything is let go.
            core.idle()
            letGo()
            warn(null)
            notices.cancel(STATUS_ID)
            status = ""
            running = false
            context.unregisterReceiver(screen)
            return
        }
        val mode = core.tick().mode

        if (power == Power.OWNER) {
            val lockedIn = core.lockedIn()
            if (hardened != lockedIn) {
                core.device.harden(lockedIn, core.texts.tr("notice.blocked"))
                hardened = lockedIn
            }
        }

        if (mode is Mode.Lock) {
            if (lockedWith != mode.allowed) {
                core.device.lock(mode.allowed)
                lockedWith = mode.allowed
            }
            val away = if (power == Power.OWNER) !core.device.isLocked() else !Watch.passes(context, Watch.front, mode.allowed)
            if (away) showLock()
        } else {
            letGo()
        }
        if (power == Power.WATCH) Watch.instance?.closeFloating(mode is Mode.Lock, (mode as? Mode.Lock)?.allowed.orEmpty(), core.verdict.blocked)

        // A break that ends is heard, so that one can come back without watching the clock.
        val breaking = mode is Mode.Lock && mode.idea != null
        if (onBreak && !breaking) notices.notify(BREAK_OVER_ID, notice(WARNINGS, "despegate", core.texts.tr("notice.break_over")).also { it.flags = it.flags and Notification.FLAG_ONGOING_EVENT.inv() })
        onBreak = breaking

        warn((mode as? Mode.Banner)?.text)
        val headline = core.headline()
        val line = headline.getString("title") + "\n" + headline.getJSONArray("details").optString(0)
        if (line != status) {
            status = line
            notices.notify(STATUS_ID, statusNotice())
        }
    }

    /** Puts the lock page in front; as the owner, also fixes the phone to it. */
    fun showLock() {
        val intent = Intent(context, LockActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
        val fixed = if (core.device.isOwner) ActivityOptions.makeBasic().setLockTaskEnabled(true).toBundle() else null
        try {
            // Only the watch may bring a screen up from the background when despegate is not the owner.
            (Watch.instance ?: context).startActivity(intent, fixed)
        } catch (e: RuntimeException) {
            Log.w("despegate", "could not show the lock page", e)
        }
    }

    private fun letGo() {
        if (lockedWith == null) return
        LockActivity.dismiss()
        core.device.unlock()
        lockedWith = null
    }

    /** A warning that stays up, counting down, for as long as it applies. */
    private fun warn(text: String?) {
        if (text == warning) return
        warning = text
        if (text == null) notices.cancel(WARNING_ID)
        else notices.notify(WARNING_ID, notice(WARNINGS, "despegate", text))
    }

    private fun notice(channel: String, title: String, text: String): Notification {
        val open = PendingIntent.getActivity(context, 0, Intent(context, MainActivity::class.java), PendingIntent.FLAG_IMMUTABLE)
        return Notification.Builder(context, channel)
            .setSmallIcon(R.drawable.ic_notice)
            .setContentTitle(title)
            .setContentText(text)
            .setStyle(Notification.BigTextStyle().bigText(text))
            .setContentIntent(open)
            .setOnlyAlertOnce(true)
            .setOngoing(channel == STATUS)
            .build()
    }

    /** Should the system have put the guard away, the next rule's start brings it back. */
    private fun wakeAtNextRule() {
        if (!core.device.isOwner) return
        val start = core.config.nextStart(LocalDateTime.now())?.second ?: return
        val intent = PendingIntent.getForegroundService(context, 1, Intent(context, Guard::class.java), PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT)
        val at = start.atZone(ZoneId.systemDefault()).toInstant().toEpochMilli()
        context.getSystemService(AlarmManager::class.java).setAndAllowWhileIdle(AlarmManager.RTC_WAKEUP, at, intent)
    }

    companion object {
        private const val TICK_MS = 2000L
        private const val STATUS = "status"
        private const val WARNINGS = "warnings"
        const val STATUS_ID = 1
        private const val WARNING_ID = 2
        private const val BREAK_OVER_ID = 3
    }
}

/**
 * What keeps despegate alive while it owns the phone: a service the system
 * does not put away. The enforcing itself is the [Enforcer]'s.
 */
class Guard : Service() {
    override fun onCreate() {
        super.onCreate()
        running = this
        startForeground(Enforcer.STATUS_ID, core.enforcer.statusNotice())
        core.enforcer.start()
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        core.enforcer.wake()
        return START_STICKY
    }

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onDestroy() {
        running = null
        super.onDestroy()
    }

    companion object {
        private var running: Guard? = null

        /** Starts the guard if despegate owns the phone; harmless when it already runs. */
        fun start(context: Context) {
            if (!context.core.device.isOwner) return
            try {
                context.startForegroundService(Intent(context, Guard::class.java))
            } catch (e: RuntimeException) {
                Log.w("despegate", "could not start the guard", e)
            }
        }

        /** Ownership is gone: the guard has nothing to stay for. */
        fun stop() {
            running?.stopSelf()
        }

        /** Has despegate look at the clock right now, after something changed. */
        fun poke(context: Context) {
            context.core.enforcer.wake()
            start(context)
        }
    }
}

/** Brings the guard back after a restart of the phone or an update of the app. */
class Boot : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) = Guard.start(context)
}
