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

/** Days of history kept. */
private const val HISTORY_DAYS = 60L

/** What is known about the phone at this instant. */
data class Sensors(
    /** Seconds since the screen was last on and unlocked. */
    val idleSecs: Long = 0,
    /** Apps with an allowance that are in front right now. */
    val inUse: List<String> = emptyList(),
    /** The lock screen is up, so the phone cannot be used. */
    val locked: Boolean = false,
)

/** How much a stretch without use has already counted for. */
enum class Rested { NOTHING, SHORT, LONG }

/** What a day added up to while breaks were on. */
data class DayLog(var workSecs: Long = 0, var breaks: Int = 0)

/**
 * Keeps count of how long the phone and individual apps have been in use, for
 * forced breaks and daily allowances, and of the breaks taken, for the history
 * of each day. It survives a restart of the app, so killing it does not reset
 * anything.
 */
class State(
    var lastSeen: LocalDateTime? = null,
    /** Use accumulated since the last proper rest. */
    var usageSecs: Long = 0,
    /** Length of the current stretch without use. */
    var restSecs: Long = 0,
    var restCounted: Rested = Rested.NOTHING,
    var breakUntil: LocalDateTime? = null,
    /** Whether the break under way is the long one. */
    var breakLong: Boolean = false,
    /** Short breaks taken since the last long one. */
    var cycle: Int = 0,
    /** The day [appSecs] refers to. */
    var day: LocalDate? = null,
    /** Time each app with an allowance has spent in front today. */
    val appSecs: MutableMap<String, Long> = sortedMapOf(),
    /** Use and breaks, day by day. */
    val history: MutableMap<LocalDate, DayLog> = sortedMapOf(),
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
            restCounted = Rested.NOTHING
            breakUntil = null
            breakLong = false
            cycle = 0
            return false
        }
        val breakSecs = policy.breakMinutes * 60L
        val longSecs = policy.longBreakMinutes * 60L
        if (breakUntil?.let { it <= now } == true) breakUntil = null

        val resting = gap || sensors.locked || breakUntil != null || sensors.idleSecs >= ACTIVE_WINDOW_SECS
        if (resting) {
            // The idle time reported is itself a stretch without use.
            restSecs = maxOf(restSecs + elapsed, sensors.idleSecs)
        } else {
            restSecs = 0
            restCounted = Rested.NOTHING
            usageSecs += elapsed
            log(now).workSecs += elapsed
        }

        // Putting the phone down for as long as a break counts as one.
        if (restSecs >= breakSecs && restCounted < Rested.SHORT) {
            restCounted = Rested.SHORT
            usageSecs = 0
            log(now).breaks += 1
            if (policy.hasLong) cycle = minOf(cycle + 1, policy.cycles - 1)
        }
        if (policy.hasLong && restSecs >= longSecs && restCounted < Rested.LONG) {
            restCounted = Rested.LONG
            cycle = 0
        }

        if (breakUntil == null && usageSecs >= policy.workMinutes * 60L) {
            val long = nextIsLong(policy)
            breakUntil = now.plusSeconds(if (long) longSecs else breakSecs)
            breakLong = long
            cycle = if (long || !policy.hasLong) 0 else cycle + 1
            usageSecs = 0
            restSecs = 0
            restCounted = if (long) Rested.LONG else Rested.SHORT
            log(now).breaks += 1
            return true
        }
        return false
    }

    /** Today's entry in the history, made if missing; old days are dropped. */
    private fun log(now: LocalDateTime): DayLog {
        val oldest = now.toLocalDate().minusDays(HISTORY_DAYS)
        history.keys.removeAll { it <= oldest }
        return history.getOrPut(now.toLocalDate()) { DayLog() }
    }

    /** Whether the next forced break is the long one. */
    fun nextIsLong(policy: BreakPolicy): Boolean = policy.hasLong && cycle + 1 >= policy.cycles

    /**
     * Whether a focus session has reached its end: the long break, or any
     * break when there are no long ones.
     */
    fun focusOver(policy: BreakPolicy?): Boolean = when {
        policy == null -> true
        policy.hasLong -> restCounted == Rested.LONG
        else -> restCounted >= Rested.SHORT
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

    /**
     * Days in a row, up to [today], with at least one break taken. A day with
     * none yet does not break the streak until it is over.
     */
    fun streak(today: LocalDate): Int {
        val took = { day: LocalDate -> (history[day]?.breaks ?: 0) > 0 }
        var day = if (took(today)) today else today.minusDays(1)
        var streak = 0
        while (took(day)) {
            streak++
            day = day.minusDays(1)
        }
        return streak
    }

    fun toJson(): JSONObject = JSONObject().apply {
        put("last_seen", lastSeen?.let(::stamp) ?: JSONObject.NULL)
        put("usage_secs", usageSecs)
        put("rest_secs", restSecs)
        put("rest_counted", restCounted.name.lowercase())
        put("break_until", breakUntil?.let(::stamp) ?: JSONObject.NULL)
        put("break_long", breakLong)
        put("cycle", cycle)
        put("day", day?.toString() ?: JSONObject.NULL)
        put("app_secs", JSONObject(appSecs.toMap()))
        put("history", JSONObject().also { o -> history.forEach { (d, l) -> o.put(d.toString(), JSONObject().put("work_secs", l.workSecs).put("breaks", l.breaks)) } })
        put("suspended", JSONArray(suspended.sorted()))
    }

    companion object {
        fun fromJson(json: JSONObject): State {
            val apps = sortedMapOf<String, Long>()
            json.optJSONObject("app_secs")?.let { o -> o.keys().forEach { apps[it] = o.optLong(it) } }
            val history = sortedMapOf<LocalDate, DayLog>()
            json.optJSONObject("history")?.let { o ->
                o.keys().forEach { key ->
                    val day = runCatching { LocalDate.parse(key) }.getOrNull() ?: return@forEach
                    val log = o.optJSONObject(key) ?: return@forEach
                    history[day] = DayLog(log.optLong("work_secs"), log.optInt("breaks"))
                }
            }
            return State(
                lastSeen = parseStamp(json.optString("last_seen")),
                usageSecs = json.optLong("usage_secs"),
                restSecs = json.optLong("rest_secs"),
                restCounted = Rested.entries.firstOrNull { it.name.lowercase() == json.optString("rest_counted") } ?: Rested.NOTHING,
                breakUntil = parseStamp(json.optString("break_until")),
                breakLong = json.optBoolean("break_long"),
                cycle = json.optInt("cycle"),
                day = runCatching { LocalDate.parse(json.optString("day")) }.getOrNull(),
                appSecs = apps,
                history = history,
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
