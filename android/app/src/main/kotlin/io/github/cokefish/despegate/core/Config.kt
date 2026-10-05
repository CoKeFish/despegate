package io.github.cokefish.despegate.core

import org.json.JSONArray
import org.json.JSONObject
import java.time.DayOfWeek
import java.time.LocalDate
import java.time.LocalDateTime
import java.time.LocalTime
import java.time.format.DateTimeFormatter

/** Monday first, under the names the page and the stored config use. */
val WEEK: List<Pair<String, DayOfWeek>> = listOf(
    "Mon" to DayOfWeek.MONDAY,
    "Tue" to DayOfWeek.TUESDAY,
    "Wed" to DayOfWeek.WEDNESDAY,
    "Thu" to DayOfWeek.THURSDAY,
    "Fri" to DayOfWeek.FRIDAY,
    "Sat" to DayOfWeek.SATURDAY,
    "Sun" to DayOfWeek.SUNDAY,
)

fun dayCode(day: DayOfWeek): String = WEEK.first { it.second == day }.first

fun dayFromCode(code: String): DayOfWeek? = WEEK.firstOrNull { it.first.equals(code, ignoreCase = true) }?.second

private val STAMP: DateTimeFormatter = DateTimeFormatter.ofPattern("yyyy-MM-dd'T'HH:mm:ss")
private val CLOCK: DateTimeFormatter = DateTimeFormatter.ofPattern("HH:mm:ss")

fun stamp(at: LocalDateTime): String = at.format(STAMP)

fun parseStamp(text: String): LocalDateTime? = runCatching { LocalDateTime.parse(text) }.getOrNull()

/** "23:00" or "23:00:00". */
fun parseClock(text: String): LocalTime? = runCatching { LocalTime.parse(text) }.getOrNull()

/**
 * After [workMinutes] of use without a proper rest, the screen locks for
 * [breakMinutes]. Putting the phone down for that long on your own counts as
 * the break.
 */
data class BreakPolicy(val workMinutes: Int, val breakMinutes: Int)

/** [app] may be in front for [minutes] per day; after that it is blocked until the next day. */
data class Allowance(val app: String, val minutes: Int)

/**
 * A recurring block: every listed day from [start] to [end]. When [end] is not
 * after [start] the window runs past midnight and belongs to the day it starts.
 */
data class Rule(
    val name: String,
    val days: List<DayOfWeek>,
    val start: LocalTime,
    val end: LocalTime,
    /** Take over the whole screen. */
    val lock: Boolean,
    /** Packages to block while the rule is active. */
    val apps: List<String>,
) {
    private fun window(day: LocalDate): Pair<LocalDateTime, LocalDateTime> {
        val from = day.atTime(start)
        val to = if (end > start) day.atTime(end) else day.plusDays(1).atTime(end)
        return from to to
    }

    /** End of the window containing [now], if the rule is active. */
    fun activeUntil(now: LocalDateTime): LocalDateTime? =
        listOf(now.toLocalDate(), now.toLocalDate().minusDays(1))
            .filter { it.dayOfWeek in days }
            .map { window(it) }
            .firstOrNull { (from, to) -> from <= now && now < to }
            ?.second

    /** Start of the next window that begins after [now]. */
    fun nextStart(now: LocalDateTime): LocalDateTime? =
        (0L..7L)
            .map { now.toLocalDate().plusDays(it) }
            .filter { it.dayOfWeek in days }
            .map { it.atTime(start) }
            .firstOrNull { it > now }
}

/** A block started on the spot. */
data class OneOff(val name: String, val until: LocalDateTime, val lock: Boolean, val apps: List<String>)

data class ActiveBlock(val name: String, val until: LocalDateTime, val lock: Boolean, val apps: List<String>)

data class Config(
    /** Language code for everything despegate says. Unset follows the phone. */
    val language: String? = null,
    /** "light", "dark" or "system" (follow the phone). */
    val appearance: String = "system",
    /** The user's own words on why despegate is installed. */
    val reasons: String = "",
    /** Photos and videos shown with the reasons; file names in the media directory. */
    val media: List<String> = emptyList(),
    /** A rule cannot be removed (nor settings weakened) this close to a block. */
    val leadMinutes: Int = 30,
    /** How long before a block the warning appears. */
    val warnMinutes: Int = 5,
    /** Length of the text to type for an emergency pause. 0 disables the pause. */
    val emergencyChars: Int = 80,
    val emergencyMinutes: Int = 5,
    val breaks: BreakPolicy? = null,
    val allowances: List<Allowance> = emptyList(),
    val rules: List<Rule> = emptyList(),
    val oneoffs: List<OneOff> = emptyList(),
    /** Packages that stay usable while the screen is locked. */
    val allowed: List<String> = emptyList(),
    /** How times of day are written: "12" with am and pm, "24" around the clock, or "system" as the phone does. */
    val clock: String = "system",
) {
    fun activeBlocks(now: LocalDateTime): List<ActiveBlock> =
        rules.mapNotNull { r -> r.activeUntil(now)?.let { ActiveBlock(r.name, it, r.lock, r.apps) } } +
            oneoffs.filter { it.until > now }.map { ActiveBlock(it.name, it.until, it.lock, it.apps) }

    /** The inactive rule that starts soonest. */
    fun nextStart(now: LocalDateTime): Pair<Rule, LocalDateTime>? =
        rules
            .filter { it.activeUntil(now) == null }
            .mapNotNull { r -> r.nextStart(now)?.let { r to it } }
            .minByOrNull { it.second }

    fun allowance(app: String): Allowance? = allowances.firstOrNull { it.app == app }

    fun toJson(): JSONObject = JSONObject().apply {
        put("language", language ?: JSONObject.NULL)
        put("appearance", appearance)
        put("reasons", reasons)
        put("media", JSONArray(media))
        put("lead_minutes", leadMinutes)
        put("warn_minutes", warnMinutes)
        put("emergency_chars", emergencyChars)
        put("emergency_minutes", emergencyMinutes)
        put("breaks", breaks?.let { JSONObject().put("work_minutes", it.workMinutes).put("break_minutes", it.breakMinutes) } ?: JSONObject.NULL)
        put("allowances", JSONArray(allowances.map { JSONObject().put("app", it.app).put("minutes", it.minutes) }))
        put("rules", JSONArray(rules.map { rule ->
            JSONObject()
                .put("name", rule.name)
                .put("days", JSONArray(rule.days.map(::dayCode)))
                .put("start", rule.start.format(CLOCK))
                .put("end", rule.end.format(CLOCK))
                .put("lock", rule.lock)
                .put("apps", JSONArray(rule.apps))
        }))
        put("oneoffs", JSONArray(oneoffs.map {
            JSONObject().put("name", it.name).put("until", stamp(it.until)).put("lock", it.lock).put("apps", JSONArray(it.apps))
        }))
        put("allowed", JSONArray(allowed))
        put("clock", clock)
    }

    companion object {
        /** Reads what [toJson] wrote; anything missing or broken falls back to the default. */
        fun fromJson(json: JSONObject): Config {
            val base = Config()
            return Config(
                language = json.optString("language").takeIf { !json.isNull("language") && it.isNotEmpty() },
                appearance = json.optString("appearance", base.appearance),
                reasons = json.optString("reasons", ""),
                media = json.strings("media"),
                leadMinutes = json.optInt("lead_minutes", base.leadMinutes),
                warnMinutes = json.optInt("warn_minutes", base.warnMinutes),
                emergencyChars = json.optInt("emergency_chars", base.emergencyChars),
                emergencyMinutes = json.optInt("emergency_minutes", base.emergencyMinutes),
                breaks = json.optJSONObject("breaks")?.let { BreakPolicy(it.optInt("work_minutes"), it.optInt("break_minutes")) },
                allowances = json.objects("allowances").map { Allowance(it.optString("app"), it.optInt("minutes")) },
                rules = json.objects("rules").mapNotNull(::ruleFromJson),
                oneoffs = json.objects("oneoffs").mapNotNull {
                    val until = parseStamp(it.optString("until")) ?: return@mapNotNull null
                    OneOff(it.optString("name"), until, it.optBoolean("lock"), it.strings("apps"))
                },
                allowed = json.strings("allowed"),
                clock = json.optString("clock", base.clock),
            )
        }

        fun ruleFromJson(json: JSONObject): Rule? {
            val start = parseClock(json.optString("start")) ?: return null
            val end = parseClock(json.optString("end")) ?: return null
            return Rule(
                name = json.optString("name"),
                days = json.strings("days").mapNotNull(::dayFromCode).distinct(),
                start = start,
                end = end,
                lock = json.optBoolean("lock"),
                apps = json.strings("apps"),
            )
        }
    }
}

fun JSONObject.strings(key: String): List<String> {
    val array = optJSONArray(key) ?: return emptyList()
    return (0 until array.length()).map { array.optString(it) }
}

fun JSONObject.objects(key: String): List<JSONObject> {
    val array = optJSONArray(key) ?: return emptyList()
    return (0 until array.length()).mapNotNull { array.optJSONObject(it) }
}
