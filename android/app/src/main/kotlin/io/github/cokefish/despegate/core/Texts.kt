package io.github.cokefish.despegate.core

import org.json.JSONObject
import java.time.DayOfWeek
import java.time.LocalDateTime
import java.time.LocalTime

/**
 * How despegate writes a time of day: the way the phone is set to, with am
 * and pm or around the clock, so that "2:40" is never left to guess.
 */
object Clock {
    @Volatile
    var twelveHour = false

    fun format(time: LocalTime): String =
        if (twelveHour) "%d:%02d %s".format(if (time.hour % 12 == 0) 12 else time.hour % 12, time.minute, if (time.hour < 12) "am" else "pm")
        else "%02d:%02d".format(time.hour, time.minute)
}

fun time(at: LocalDateTime): String = Clock.format(at.toLocalTime())

/** Everything despegate says in one language: a catalog of texts with `{placeholders}`. */
class Texts(val code: String, private val catalog: Map<String, String>) {
    /** The language's own name for itself. */
    val name: String get() = catalog["lang.name"] ?: code

    fun tr(key: String, vararg vars: Pair<String, Any?>): String {
        var text = catalog[key] ?: return key
        for ((name, value) in vars) text = text.replace("{$name}", value.toString())
        return text
    }

    fun has(key: String): Boolean = key in catalog

    /** The texts whose key starts with any of [prefixes], for a page to carry. */
    fun section(vararg prefixes: String): JSONObject {
        val out = JSONObject()
        for ((key, text) in catalog) if (prefixes.any { key.startsWith(it) }) out.put(key, text)
        return out
    }

    fun dayName(day: DayOfWeek): String = tr("day." + dayCode(day).lowercase())

    /** Day and time, e.g. "lun 07:00". */
    fun whenAt(at: LocalDateTime): String = "${dayName(at.dayOfWeek)} ${time(at)}"

    companion object {
        fun fromJson(code: String, json: JSONObject): Texts {
            val catalog = HashMap<String, String>()
            json.keys().forEach { catalog[it] = json.optString(it) }
            return Texts(code, catalog)
        }
    }
}

/** The languages despegate speaks, and the one the phone itself is set to. */
class Langs(val all: List<Texts>, systemCode: String) {
    val system: Texts = byCode(systemCode) ?: all.first()

    /** "es-CO" and "ES_es" are both Spanish. */
    fun byCode(code: String): Texts? {
        val short = code.trim().lowercase().split('-', '_').first()
        return all.firstOrNull { it.code == short }
    }

    /** The configured language, else the phone's. */
    fun resolve(configured: String?): Texts = configured?.let(::byCode) ?: system
}
