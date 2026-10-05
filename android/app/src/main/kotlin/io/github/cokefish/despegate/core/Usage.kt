package io.github.cokefish.despegate.core

import org.json.JSONArray
import org.json.JSONObject
import java.time.Duration
import java.time.LocalDate
import java.time.LocalDateTime

/** Use within the last minute counts as using the phone. */
const val ACTIVE_WINDOW_SECS = 60L

/**
 * A longer gap between two updates means the screen or the app was off, which
 * is rest, not use.
 */
private const val MAX_STEP_SECS = 10L

/** What is known about the phone at this instant. */
data class Sensors(
    /** Seconds since the screen was last on and unlocked. */
    val idleSecs: Long = 0,
    /** Apps with an allowance that are in front right now. */
    val inUse: List<String> = emptyList(),
    /** The lock screen is up, so the phone cannot be used. */
    val locked: Boolean = false,
)

/**
 * Keeps count of how long the phone and individual apps have been in use, for
 * forced breaks and daily allowances. It survives a restart of the app, so
 * killing it does not reset anything.
 */
class State(
    var lastSeen: LocalDateTime? = null,
    /** Use accumulated since the last proper rest. */
    var usageSecs: Long = 0,
    /** Length of the current stretch without use. */
    var restSecs: Long = 0,
    var breakUntil: LocalDateTime? = null,
    /** The day [appSecs] refers to. */
    var day: LocalDate? = null,
    /** Time each app with an allowance has spent in front today. */
    val appSecs: MutableMap<String, Long> = sortedMapOf(),
    /** Packages despegate has suspended and must give back. */
    var suspended: Set<String> = emptySet(),
) {
    /** Moves the clocks forward to [now]. Returns true when a break starts. */
    fun advance(config: Config, now: LocalDateTime, sensors: Sensors): Boolean {
        val elapsed = lastSeen?.let { Duration.between(it, now).seconds.coerceAtLeast(0) } ?: 0
        lastSeen = now
        val gap = elapsed > MAX_STEP_SECS

        if (day != now.toLocalDate()) {
            day = now.toLocalDate()
            appSecs.clear()
        }
        if (!gap && !sensors.locked) {
            for (app in sensors.inUse) appSecs[app] = (appSecs[app] ?: 0) + elapsed
        }

        val policy = config.breaks
        if (policy == null) {
            usageSecs = 0
            restSecs = 0
            breakUntil = null
            return false
        }
        val breakSecs = policy.breakMinutes * 60L
        if (breakUntil?.let { it <= now } == true) breakUntil = null

        val resting = gap || sensors.locked || breakUntil != null || sensors.idleSecs >= ACTIVE_WINDOW_SECS
        if (resting) {
            // The idle time reported is itself a stretch without use.
            restSecs = maxOf(restSecs + elapsed, sensors.idleSecs)
        } else {
            restSecs = 0
            usageSecs += elapsed
        }
        if (restSecs >= breakSecs) usageSecs = 0

        if (breakUntil == null && usageSecs >= policy.workMinutes * 60L) {
            breakUntil = now.plusSeconds(breakSecs)
            usageSecs = 0
            restSecs = 0
            return true
        }
        return false
    }

    fun onBreak(now: LocalDateTime): LocalDateTime? = breakUntil?.takeIf { it > now }

    /** Use left before the next forced break. */
    fun breakDueIn(config: Config): Duration? =
        config.breaks?.let { Duration.ofSeconds((it.workMinutes * 60L - usageSecs).coerceAtLeast(0)) }

    fun usedToday(app: String, now: LocalDateTime): Long =
        if (day != now.toLocalDate()) 0 else appSecs[app] ?: 0

    /** Time [app] may still be used today, if it has an allowance. */
    fun allowanceLeft(config: Config, app: String, now: LocalDateTime): Duration? =
        config.allowance(app)?.let { Duration.ofSeconds((it.minutes * 60L - usedToday(app, now)).coerceAtLeast(0)) }

    /** Apps whose allowance for today is spent. */
    fun exhausted(config: Config, now: LocalDateTime): List<String> =
        config.allowances.filter { usedToday(it.app, now) >= it.minutes * 60L }.map { it.app }

    fun toJson(): JSONObject = JSONObject().apply {
        put("last_seen", lastSeen?.let(::stamp) ?: JSONObject.NULL)
        put("usage_secs", usageSecs)
        put("rest_secs", restSecs)
        put("break_until", breakUntil?.let(::stamp) ?: JSONObject.NULL)
        put("day", day?.toString() ?: JSONObject.NULL)
        put("app_secs", JSONObject(appSecs.toMap()))
        put("suspended", JSONArray(suspended.sorted()))
    }

    companion object {
        fun fromJson(json: JSONObject): State {
            val apps = sortedMapOf<String, Long>()
            json.optJSONObject("app_secs")?.let { o -> o.keys().forEach { apps[it] = o.optLong(it) } }
            return State(
                lastSeen = parseStamp(json.optString("last_seen")),
                usageSecs = json.optLong("usage_secs"),
                restSecs = json.optLong("rest_secs"),
                breakUntil = parseStamp(json.optString("break_until")),
                day = runCatching { LocalDate.parse(json.optString("day")) }.getOrNull(),
                appSecs = apps,
                suspended = json.strings("suspended").toSet(),
            )
        }
    }
}

/** "1 h 05 min" or "4:59", for warnings and the lock screen. */
fun countdown(left: Duration): String {
    val seconds = left.seconds.coerceAtLeast(0)
    return if (seconds >= 3600) "%d h %02d min".format(seconds / 3600, seconds % 3600 / 60)
    else "%d:%02d".format(seconds / 60, seconds % 60)
}
