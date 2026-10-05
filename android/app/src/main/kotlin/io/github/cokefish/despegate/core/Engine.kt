package io.github.cokefish.despegate.core

import org.json.JSONArray
import org.json.JSONObject
import java.security.SecureRandom
import java.time.Duration
import java.time.LocalDateTime
import java.util.Random

/** What the phone must be showing right now. */
sealed interface Mode {
    data object Idle : Mode

    /** A warning about what is about to happen. */
    data class Banner(val text: String) : Mode

    /** The whole screen taken over, with only [allowed] still usable. */
    data class Lock(
        val until: LocalDateTime,
        val reasons: String,
        val media: List<String>,
        /** Text to type for an emergency pause, when there is one. */
        val challenge: String?,
        val emergencyMinutes: Int,
        val allowed: List<String>,
        /** Something to do with the time, when the lock is a break. */
        val idea: String? = null,
    ) : Mode
}

/** One round's decision: what to show, and which apps to keep blocked. */
data class Verdict(val mode: Mode, val blocked: Set<String>)

/**
 * Decides, from the config and the clocks, what to enforce. It holds the two
 * things that do not survive a restart: the emergency pause and the text that
 * earns it.
 */
class Engine(private val random: Random = SecureRandom()) {
    var pausedUntil: LocalDateTime? = null
        private set
    var challenge: String? = null
        private set

    fun decide(config: Config, state: State, sensors: Sensors, now: LocalDateTime, t: Texts, label: (String) -> String = { it }): Verdict {
        if (pausedUntil?.let { it <= now } == true) pausedUntil = null

        val blocks = config.activeBlocks(now).toMutableList()
        state.onBreak(now)?.let { blocks += ActiveBlock("break", it, lock = true, apps = emptyList()) }
        val exhausted = state.exhausted(config, now)
        // A focus session keeps its apps blocked while working; breaks let them through.
        val focusing = config.focus?.takeIf { state.onBreak(now) == null }?.apps.orEmpty()
        val lockUntil = blocks.filter { it.lock }.maxOfOrNull { it.until }
        val enforcing = blocks.isNotEmpty() || exhausted.isNotEmpty() || focusing.isNotEmpty()

        val paused = pausedUntil
        val verdict = when {
            paused != null && enforcing ->
                Verdict(Mode.Banner(t.tr("banner.paused", "left" to countdown(Duration.between(now, paused)))), emptySet())
            paused != null -> Verdict(warning(config, state, sensors, now, t, label), emptySet())
            else -> {
                val blocked = blocks.flatMap { it.apps }.toSet() + exhausted + focusing
                val mode = if (lockUntil != null) {
                    if (challenge == null && config.emergencyChars > 0) challenge = newChallenge(config.emergencyChars)
                    val idea = state.onBreak(now)?.let { t.tr("break.idea_${Math.floorMod(it.toLocalTime().toSecondOfDay() / 60, IDEAS) + 1}") }
                    Mode.Lock(lockUntil, config.reasons, config.media, challenge, config.emergencyMinutes, config.allowed - blocked, idea)
                } else {
                    warning(config, state, sensors, now, t, label)
                }
                Verdict(mode, blocked)
            }
        }
        if (lockUntil == null) challenge = null
        return verdict
    }

    /**
     * What has been typed on the lock screen. Typing the challenge exactly
     * earns the emergency pause; returns whether it just did.
     */
    fun typed(text: String, config: Config, now: LocalDateTime): Boolean {
        if (challenge == null || challenge != text) return false
        pausedUntil = now.plusMinutes(config.emergencyMinutes.toLong())
        challenge = null
        return true
    }

    /** A warning about whatever is about to happen soonest, or nothing. */
    private fun warning(config: Config, state: State, sensors: Sensors, now: LocalDateTime, t: Texts, label: (String) -> String): Mode {
        val warn = Duration.ofMinutes(config.warnMinutes.toLong())
        val warnings = ArrayList<Pair<Duration, String>>()
        config.nextStart(now)?.let { (rule, start) ->
            val left = Duration.between(now, start)
            warnings += left to t.tr("banner.rule_soon", "name" to rule.name, "left" to countdown(left))
        }
        if (state.onBreak(now) == null) {
            state.breakDueIn(config)?.let { warnings += it to t.tr("banner.break_soon", "left" to countdown(it)) }
        }
        for (app in sensors.inUse) {
            state.allowanceLeft(config, app, now)?.let {
                warnings += it to t.tr("banner.allowance_soon", "app" to label(app), "left" to countdown(it))
            }
        }
        return warnings.filter { it.first <= warn }.minByOrNull { it.first }?.let { Mode.Banner(it.second) } ?: Mode.Idle
    }

    private companion object {
        /** How many break ideas the catalogs hold (`break.idea_1` and on). */
        const val IDEAS = 8
    }

    /** Groups of five characters, without the ones that are easily confused. */
    private fun newChallenge(length: Int): String {
        val alphabet = "abcdefghjkmnpqrstuvwxyz23456789"
        return (0 until length)
            .map { if (it % 6 == 5) ' ' else alphabet[random.nextInt(alphabet.length)] }
            .joinToString("")
            .trimEnd()
    }
}

/** What the settings page shows first: what is happening now and what comes next. */
object Headline {
    fun of(config: Config, state: State, running: Boolean, engine: Engine, now: LocalDateTime, t: Texts, label: (String) -> String): JSONObject {
        if (!running) return make("stopped", t.tr("ui.hero.stopped"), listOf(t.tr("ui.hero.stopped_detail")))

        val blocks = config.activeBlocks(now)
        val onBreak = state.onBreak(now)
        val lock = blocks.filter { it.lock }.maxByOrNull { it.until }
        val (kind, title) = when {
            engine.pausedUntil?.let { it > now } == true -> "paused" to t.tr("ui.hero.paused")
            onBreak != null -> "break" to t.tr("ui.hero.on_break", "until" to time(onBreak))
            lock != null -> "locked" to t.tr("ui.hero.locked", "until" to t.whenAt(lock.until))
            blocks.isNotEmpty() -> {
                val apps = blocks.flatMap { it.apps }.distinct().joinToString(", ", transform = label)
                "blocking" to t.tr("ui.hero.blocking", "apps" to apps, "until" to t.whenAt(blocks.maxOf { it.until }))
            }
            config.focus != null -> "focus" to t.tr("ui.hero.focus")
            else -> "free" to t.tr("ui.hero.free")
        }

        val details = ArrayList<String>()
        val next = config.nextStart(now)
        if (next != null) {
            val (rule, start) = next
            details += t.tr("ui.hero.next", "name" to rule.name, "when" to t.whenAt(start), "left" to countdown(Duration.between(now, start)))
        } else if (config.rules.isEmpty() && config.breaks == null && config.allowances.isEmpty()) {
            details += t.tr("ui.hero.empty")
        }
        if (config.breaks != null && onBreak == null) {
            state.breakDueIn(config)?.let { details += t.tr("ui.hero.break_in", "left" to countdown(it)) }
        }
        return make(kind, title, details)
    }

    private fun make(kind: String, title: String, details: List<String>): JSONObject =
        JSONObject().put("kind", kind).put("title", title).put("details", JSONArray(details))
}
