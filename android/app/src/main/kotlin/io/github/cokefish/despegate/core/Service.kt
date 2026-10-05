package io.github.cokefish.despegate.core

import org.json.JSONObject
import java.time.Duration
import java.time.LocalDateTime

/** What is known beyond the config when a request arrives. */
class Runtime(
    val state: State,
    /** Packages that must never be blocked: despegate itself, the dialer, the system. */
    val protectedApps: Set<String> = emptySet(),
    /** The name an app goes by on the phone, for messages. */
    val label: (String) -> String = { it },
)

/** The answer to a request. [config] is the old one unless the request was accepted. */
data class Outcome(val ok: Boolean, val message: String, val config: Config)

private class Refused(message: String) : Exception(message)

/**
 * Carries out the changes the settings page asks for. Whatever loosens
 * despegate is refused while a block is active or about to start.
 */
object Service {
    private const val MAX_NOW_MINUTES = 24 * 60
    private val PACKAGE = Regex("[A-Za-z0-9_]+(\\.[A-Za-z0-9_]+)+")

    fun handle(request: JSONObject, cfg: Config, now: LocalDateTime, rt: Runtime, langs: Langs): Outcome {
        val t = langs.resolve(cfg.language)
        return try {
            val (config, message) = when (request.optString("cmd")) {
                "rule_add" -> ruleAdd(cfg, request.optJSONObject("rule") ?: JSONObject(), now, rt, t)
                "rule_remove" -> ruleRemove(cfg, request.optString("name"), now, t)
                "now" -> startNow(cfg, request.optInt("minutes"), request.optBoolean("lock"), request.strings("apps"), now, rt, t)
                "reasons_set" -> cfg.copy(reasons = request.optString("text").trim()) to t.tr("reasons.saved")
                "set" -> set(cfg, request.optString("key"), request.optInt("value", -1), now, rt, t)
                "break_set" -> breakSet(
                    cfg,
                    BreakPolicy(request.optInt("work_minutes"), request.optInt("break_minutes"), request.optInt("long_break_minutes"), request.optInt("cycles", 4)),
                    now, rt, t,
                )
                "break_off" -> breakSet(cfg, null, now, rt, t)
                "focus_start" -> focusStart(cfg, request.strings("apps"), now, rt, t)
                "focus_stop" -> {
                    if (cfg.focus == null) throw Refused(t.tr("error.no_focus"))
                    mayLoosen(cfg, now, rt, t)
                    cfg.copy(focus = null) to t.tr("focus.stopped")
                }
                "allowance_set" -> allowanceSet(cfg, request.optString("app"), request.optInt("minutes"), now, rt, t)
                "allowance_remove" -> allowanceSet(cfg, request.optString("app"), null, now, rt, t)
                "allowed_set" -> allowedSet(cfg, request.strings("apps"), now, rt, t)
                "language_set" -> languageSet(cfg, request.optString("code").takeIf { !request.isNull("code") && it != "auto" }, t, langs)
                "appearance_set" -> appearanceSet(cfg, request.optString("mode"), t)
                "clock_set" -> {
                    val mode = request.optString("mode")
                    if (mode !in listOf("system", "12", "24")) throw Refused(t.tr("error.unknown", "cmd" to mode))
                    cfg.copy(clock = mode) to t.tr("set.done")
                }
                "media_add" -> {
                    val name = request.optString("name")
                    cfg.copy(media = (cfg.media + name).distinct()) to t.tr("media.added", "name" to name)
                }
                "media_remove" -> {
                    val name = request.optString("name")
                    if (name !in cfg.media) throw Refused(t.tr("error.no_media", "name" to name))
                    cfg.copy(media = cfg.media - name) to t.tr("media.removed", "name" to name)
                }
                else -> throw Refused(t.tr("error.unknown", "cmd" to request.optString("cmd")))
            }
            Outcome(true, message, config)
        } catch (refused: Refused) {
            Outcome(false, refused.message.orEmpty(), cfg)
        }
    }

    /** Drops one-off blocks that have ended. */
    fun prune(cfg: Config, now: LocalDateTime): Config =
        if (cfg.oneoffs.all { it.until > now }) cfg else cfg.copy(oneoffs = cfg.oneoffs.filter { it.until > now })

    /**
     * Why loosening anything is currently forbidden, if it is: a block or break
     * is active, or one is about to start.
     */
    fun lockedIn(cfg: Config, state: State, now: LocalDateTime, t: Texts): String? {
        cfg.activeBlocks(now).firstOrNull()?.let {
            return t.tr("why.block_active", "name" to it.name, "until" to t.whenAt(it.until))
        }
        state.onBreak(now)?.let { return t.tr("why.break_active", "until" to time(it)) }
        cfg.nextStart(now)?.let { (rule, start) ->
            if (Duration.between(now, start) <= Duration.ofMinutes(cfg.leadMinutes.toLong())) {
                return t.tr("why.block_imminent", "name" to rule.name, "start" to time(start), "lead" to cfg.leadMinutes)
            }
        }
        // Breaks are a choice, not a sentence: they close in only once their
        // warning is up. Short work periods shrink that further.
        val policy = cfg.breaks ?: return null
        val lead = minOf(cfg.warnMinutes, policy.workMinutes / 2)
        val due = state.breakDueIn(cfg) ?: return null
        return if (due <= Duration.ofMinutes(lead.toLong())) t.tr("why.break_imminent", "left" to countdown(due)) else null
    }

    fun describeEffect(lock: Boolean, apps: List<String>, t: Texts, label: (String) -> String): String {
        val names = apps.joinToString(", ", transform = label)
        return when {
            lock && apps.isEmpty() -> t.tr("effect.lock")
            lock -> t.tr("effect.lock_and_close", "apps" to names)
            else -> t.tr("effect.close", "apps" to names)
        }
    }

    fun describeDays(rule: Rule, t: Texts): String =
        if (rule.days.size == 7) t.tr("days.every")
        else WEEK.map { it.second }.filter { it in rule.days }.joinToString(", ") { t.dayName(it) }

    private fun describeRule(rule: Rule, t: Texts, label: (String) -> String): String =
        t.tr(
            "rule.describe",
            "name" to rule.name,
            "days" to describeDays(rule, t),
            "start" to Clock.format(rule.start),
            "end" to Clock.format(rule.end),
            "effect" to describeEffect(rule.lock, rule.apps, t, label),
        )

    private fun appName(app: String, rt: Runtime, t: Texts): String {
        val name = app.trim()
        if (name.isEmpty()) throw Refused(t.tr("error.app_empty"))
        if (!PACKAGE.matches(name)) throw Refused(t.tr("error.app_unknown", "app" to name))
        if (name in rt.protectedApps) throw Refused(t.tr("error.app_protected", "app" to rt.label(name)))
        return name
    }

    private fun normalizeApps(apps: List<String>, rt: Runtime, t: Texts): List<String> =
        apps.map { appName(it, rt, t) }.distinct()

    private fun mayLoosen(cfg: Config, now: LocalDateTime, rt: Runtime, t: Texts) {
        lockedIn(cfg, rt.state, now, t)?.let { throw Refused(t.tr("error.loosens", "why" to it)) }
    }

    private fun ruleAdd(cfg: Config, json: JSONObject, now: LocalDateTime, rt: Runtime, t: Texts): Pair<Config, String> {
        val name = json.optString("name").trim()
        if (name.isEmpty()) throw Refused(t.tr("error.name_required"))
        if (cfg.rules.any { it.name == name }) throw Refused(t.tr("error.name_taken", "name" to name))
        val days = json.strings("days").mapNotNull(::dayFromCode).distinct()
        if (days.isEmpty()) throw Refused(t.tr("error.days_required"))
        val start = parseClock(json.optString("start"))
        val end = parseClock(json.optString("end"))
        if (start == null || end == null) throw Refused(t.tr("error.time_required"))
        if (start == end) throw Refused(t.tr("error.same_time"))
        val apps = normalizeApps(json.strings("apps"), rt, t)
        val lock = json.optBoolean("lock")
        if (!lock && apps.isEmpty()) throw Refused(t.tr("error.no_effect"))
        val rule = Rule(name, days, start, end, lock, apps)
        var message = t.tr("rule.added", "rule" to describeRule(rule, t, rt.label))
        rule.activeUntil(now)?.let { message += "\n" + t.tr("rule.active_now", "until" to t.whenAt(it)) }
        return cfg.copy(rules = cfg.rules + rule) to message
    }

    private fun ruleRemove(cfg: Config, name: String, now: LocalDateTime, t: Texts): Pair<Config, String> {
        val rule = cfg.rules.firstOrNull { it.name == name } ?: throw Refused(t.tr("error.no_rule", "name" to name))
        rule.activeUntil(now)?.let {
            throw Refused(t.tr("error.rule_active", "name" to name, "until" to t.whenAt(it)))
        }
        rule.nextStart(now)?.let { start ->
            if (Duration.between(now, start) <= Duration.ofMinutes(cfg.leadMinutes.toLong())) {
                throw Refused(t.tr("error.rule_imminent", "name" to name, "start" to time(start), "lead" to cfg.leadMinutes))
            }
        }
        return cfg.copy(rules = cfg.rules - rule) to t.tr("rule.removed", "name" to name)
    }

    private fun startNow(cfg: Config, minutes: Int, lock: Boolean, apps: List<String>, now: LocalDateTime, rt: Runtime, t: Texts): Pair<Config, String> {
        if (minutes <= 0 || minutes > MAX_NOW_MINUTES) throw Refused(t.tr("error.duration_range", "hours" to MAX_NOW_MINUTES / 60))
        val blocked = normalizeApps(apps, rt, t)
        if (!lock && blocked.isEmpty()) throw Refused(t.tr("error.nothing_to_block"))
        val until = now.plusMinutes(minutes.toLong())
        val base = "now-%02d:%02d".format(until.hour, until.minute)
        var name = base
        var n = 2
        while (cfg.oneoffs.any { it.name == name }) name = "$base-${n++}"
        val message = t.tr("now.started", "effect" to describeEffect(lock, blocked, t, rt.label), "until" to t.whenAt(until))
        return cfg.copy(oneoffs = cfg.oneoffs + OneOff(name, until, lock, blocked)) to message
    }

    private fun set(cfg: Config, key: String, value: Int, now: LocalDateTime, rt: Runtime, t: Texts): Pair<Config, String> {
        val old = when (key) {
            "lead-minutes" -> cfg.leadMinutes
            "warn-minutes" -> cfg.warnMinutes
            "emergency-chars" -> cfg.emergencyChars
            "emergency-minutes" -> cfg.emergencyMinutes
            else -> throw Refused(t.tr("error.unknown", "cmd" to key))
        }
        val (max, weaker) = when (key) {
            "lead-minutes" -> 24 * 60 to (value < old)
            "warn-minutes" -> 60 to false
            // 0 disables the pause entirely, so it is the strictest value.
            "emergency-chars" -> 2000 to (value != 0 && (old == 0 || value < old))
            else -> 60 to (value > old)
        }
        if (value < 0 || value > max) throw Refused(t.tr("error.max", "max" to max))
        if (key == "emergency-minutes" && value == 0) throw Refused(t.tr("error.emergency_min"))
        // Checked before applying, so lowering the lead time cannot unlock itself.
        if (weaker) mayLoosen(cfg, now, rt, t)
        val config = when (key) {
            "lead-minutes" -> cfg.copy(leadMinutes = value)
            "warn-minutes" -> cfg.copy(warnMinutes = value)
            "emergency-chars" -> cfg.copy(emergencyChars = value)
            else -> cfg.copy(emergencyMinutes = value)
        }
        var message = t.tr("set.done")
        if (key == "emergency-chars" && value == 0) message += "\n" + t.tr("set.no_emergency_warning")
        return config to message
    }

    private fun breakSet(cfg: Config, policy: BreakPolicy?, now: LocalDateTime, rt: Runtime, t: Texts): Pair<Config, String> {
        if (policy != null) {
            if (policy.workMinutes !in 5..480) throw Refused(t.tr("error.break_work_range"))
            if (policy.breakMinutes !in 1..120) throw Refused(t.tr("error.break_rest_range"))
            if (policy.longBreakMinutes != 0 && policy.longBreakMinutes !in policy.breakMinutes + 1..180) throw Refused(t.tr("error.break_long_range"))
            if (policy.longBreakMinutes != 0 && policy.cycles !in 2..12) throw Refused(t.tr("error.break_cycles_range"))
        }
        val old = cfg.breaks
        val weaker = when {
            old != null && policy != null -> {
                // A long break is loosened by shortening it, spacing it out or dropping it.
                val longLooser = old.hasLong && (!policy.hasLong || policy.longBreakMinutes < old.longBreakMinutes || policy.cycles > old.cycles)
                policy.workMinutes > old.workMinutes || policy.breakMinutes < old.breakMinutes || longLooser
            }
            old != null -> true
            policy != null -> false
            else -> throw Refused(t.tr("error.break_already_off"))
        }
        if (weaker) mayLoosen(cfg, now, rt, t)
        val message = when {
            policy == null -> t.tr("break.off")
            policy.hasLong -> t.tr("break.set_long", "rest" to policy.breakMinutes, "work" to policy.workMinutes, "long" to policy.longBreakMinutes, "cycles" to policy.cycles)
            else -> t.tr("break.set", "rest" to policy.breakMinutes, "work" to policy.workMinutes)
        }
        return cfg.copy(breaks = policy, focus = if (policy == null) null else cfg.focus) to message
    }

    /** Starting a focus session, or adding apps to one, only tightens. */
    private fun focusStart(cfg: Config, apps: List<String>, now: LocalDateTime, rt: Runtime, t: Texts): Pair<Config, String> {
        val policy = cfg.breaks ?: throw Refused(t.tr("error.focus_needs_breaks"))
        val added = normalizeApps(apps, rt, t)
        if (added.isEmpty()) throw Refused(t.tr("error.focus_no_apps"))
        val old = cfg.focus
        val all = (old?.apps.orEmpty() + added).distinct()
        val names = all.joinToString(", ", transform = rt.label)
        val message = t.tr(if (policy.hasLong) "focus.started_long" else "focus.started", "apps" to names)
        return cfg.copy(focus = Focus(all, old?.started ?: now)) to message
    }

    private fun allowanceSet(cfg: Config, app: String, minutes: Int?, now: LocalDateTime, rt: Runtime, t: Texts): Pair<Config, String> {
        val name = appName(app, rt, t)
        val old = cfg.allowance(name)?.minutes
        if (minutes != null && minutes !in 1..24 * 60) throw Refused(t.tr("error.allowance_range"))
        val weaker = when {
            old != null && minutes != null -> minutes > old
            old != null -> true
            minutes != null -> false
            else -> throw Refused(t.tr("error.no_allowance", "app" to rt.label(name)))
        }
        if (weaker) {
            mayLoosen(cfg, now, rt, t)
            // Otherwise the limit could be raised the moment it is reached.
            val used = rt.state.usedToday(name, now)
            if (used > 0) {
                throw Refused(t.tr("error.allowance_used", "app" to rt.label(name), "used" to countdown(Duration.ofSeconds(used))))
            }
        }
        val rest = cfg.allowances.filter { it.app != name }
        return if (minutes != null) {
            cfg.copy(allowances = rest + Allowance(name, minutes)) to t.tr("allowance.set", "app" to rt.label(name), "minutes" to minutes)
        } else {
            cfg.copy(allowances = rest) to t.tr("allowance.removed", "app" to rt.label(name))
        }
    }

    /** Letting another app through a locked screen loosens it; taking one away does not. */
    private fun allowedSet(cfg: Config, apps: List<String>, now: LocalDateTime, rt: Runtime, t: Texts): Pair<Config, String> {
        val allowed = apps.map { it.trim() }.filter { PACKAGE.matches(it) }.distinct()
        if (allowed.any { it !in cfg.allowed }) mayLoosen(cfg, now, rt, t)
        return cfg.copy(allowed = allowed) to t.tr("allowed.set", "count" to allowed.size)
    }

    private fun languageSet(cfg: Config, code: String?, t: Texts, langs: Langs): Pair<Config, String> {
        if (code == null) return cfg.copy(language = null) to langs.system.tr("language.auto", "name" to langs.system.name)
        val chosen = langs.byCode(code)
            ?: throw Refused(t.tr("error.language", "code" to code, "list" to langs.all.joinToString(", ") { it.code }))
        return cfg.copy(language = chosen.code) to chosen.tr("language.set", "name" to chosen.name)
    }

    private fun appearanceSet(cfg: Config, mode: String, t: Texts): Pair<Config, String> {
        if (mode !in listOf("system", "light", "dark")) throw Refused(t.tr("error.unknown", "cmd" to mode))
        return cfg.copy(appearance = mode) to t.tr("appearance.set", "mode" to t.tr("appearance.$mode"))
    }
}
