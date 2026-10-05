package io.github.cokefish.despegate

import io.github.cokefish.despegate.core.Allowance
import io.github.cokefish.despegate.core.BreakPolicy
import io.github.cokefish.despegate.core.Clock
import io.github.cokefish.despegate.core.Config
import io.github.cokefish.despegate.core.Engine
import io.github.cokefish.despegate.core.Langs
import io.github.cokefish.despegate.core.Mode
import io.github.cokefish.despegate.core.Rule
import io.github.cokefish.despegate.core.Runtime
import io.github.cokefish.despegate.core.Sensors
import io.github.cokefish.despegate.core.Service
import io.github.cokefish.despegate.core.State
import io.github.cokefish.despegate.core.Texts
import org.json.JSONArray
import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import java.io.File
import java.time.DayOfWeek
import java.time.LocalDateTime
import java.time.LocalTime

class CoreTest {
    private val assets = File("src/main/assets")
    private val catalogs = listOf("en", "es").associateWith { JSONObject(File(assets, "locales/$it.json").readText()) }
    private val langs = Langs(catalogs.map { (code, json) -> Texts.fromJson(code, json) }, "en")
    private val en = langs.system

    /** A Wednesday. */
    private fun at(hour: Int, minute: Int = 0, day: Int = 7): LocalDateTime = LocalDateTime.of(2026, 10, day, hour, minute)

    private val sleep = Rule("sleep", DayOfWeek.entries, LocalTime.of(23, 0), LocalTime.of(7, 0), lock = true, apps = emptyList())
    private val games = Rule("games", listOf(DayOfWeek.WEDNESDAY), LocalTime.of(9, 0), LocalTime.of(13, 0), lock = false, apps = listOf("com.example.game"))

    private fun ask(cfg: Config, state: State, now: LocalDateTime, cmd: String, vararg fields: Pair<String, Any>) =
        Service.handle(JSONObject(mapOf("cmd" to cmd, *fields)), cfg, now, Runtime(state, setOf("com.android.dialer")), langs)

    @Test
    fun aRulePastMidnightBelongsToTheDayItStarts() {
        assertEquals(at(7, day = 8), sleep.activeUntil(at(23, 30)))
        assertEquals(at(7), sleep.activeUntil(at(2)))
        assertNull(sleep.activeUntil(at(12)))
        assertEquals(at(23), sleep.nextStart(at(12)))
    }

    @Test
    fun timesAreWrittenTheWayThePhoneIsSetTo() {
        try {
            assertEquals("23:05", Clock.format(LocalTime.of(23, 5)))
            Clock.twelveHour = true
            assertEquals("11:05 pm", Clock.format(LocalTime.of(23, 5)))
            assertEquals("12:00 am", Clock.format(LocalTime.of(0, 0)))
            assertEquals("12:30 pm", Clock.format(LocalTime.of(12, 30)))
            assertEquals("7:00 am", Clock.format(LocalTime.of(7, 0)))
        } finally {
            Clock.twelveHour = false
        }
    }

    @Test
    fun theConfigSurvivesBeingWrittenAndRead() {
        val cfg = Config(reasons = "Sleep.", breaks = BreakPolicy(50, 10), allowances = listOf(Allowance("com.example.game", 30)), rules = listOf(sleep, games), allowed = listOf("com.example.chat"))
        assertEquals(cfg, Config.fromJson(JSONObject(cfg.toJson().toString())))
    }

    @Test
    fun aLockRuleLocksAndAnAppRuleBlocksItsApps() {
        val engine = Engine()
        val cfg = Config(rules = listOf(sleep, games), allowed = listOf("com.example.chat"))
        val locked = engine.decide(cfg, State(), Sensors(), at(23, 30), en)
        val lock = locked.mode as Mode.Lock
        assertEquals(at(7, day = 8), lock.until)
        assertEquals(listOf("com.example.chat"), lock.allowed)
        assertNotNull(lock.challenge)

        val blocking = engine.decide(cfg, State(), Sensors(), at(10), en)
        assertEquals(setOf("com.example.game"), blocking.blocked)
        assertFalse(blocking.mode is Mode.Lock)
    }

    @Test
    fun typingTheChallengeEarnsAPauseThatLiftsEverything() {
        val engine = Engine()
        val cfg = Config(rules = listOf(sleep))
        val lock = engine.decide(cfg, State(), Sensors(), at(23, 30), en).mode as Mode.Lock
        assertFalse(engine.typed("something else", cfg, at(23, 31)))
        assertTrue(engine.typed(lock.challenge!!, cfg, at(23, 31)))

        val paused = engine.decide(cfg, State(), Sensors(), at(23, 32), en)
        assertTrue(paused.mode is Mode.Banner)
        assertTrue(paused.blocked.isEmpty())
        // The pause runs out and the lock comes back, with a new text to type.
        val again = engine.decide(cfg, State(), Sensors(), at(23, 37), en).mode as Mode.Lock
        assertNotNull(again.challenge)
    }

    @Test
    fun aBreakStartsAfterEnoughUseAndRestResetsTheCount() {
        val cfg = Config(breaks = BreakPolicy(workMinutes = 5, breakMinutes = 1))
        val state = State()
        var now = at(10)
        var started = false
        repeat(5 * 60 / 5 + 1) {
            started = state.advance(cfg, now, Sensors(idleSecs = 0)) || started
            now = now.plusSeconds(5)
        }
        assertTrue(started)
        assertNotNull(state.onBreak(now.minusSeconds(5)))

        val rested = State()
        rested.advance(cfg, at(10), Sensors())
        rested.advance(cfg, at(10).plusSeconds(8), Sensors())
        assertEquals(8, rested.usageSecs)
        // The screen was off for two minutes: longer than the break.
        rested.advance(cfg, at(10, 2), Sensors(idleSecs = 120))
        assertEquals(0, rested.usageSecs)
    }

    @Test
    fun aSpentAllowanceBlocksTheAppUntilTheNextDay() {
        val cfg = Config(allowances = listOf(Allowance("com.example.game", 1)))
        val state = State()
        var now = at(10)
        repeat(14) {
            state.advance(cfg, now, Sensors(inUse = listOf("com.example.game")))
            now = now.plusSeconds(5)
        }
        assertEquals(setOf("com.example.game"), Engine().decide(cfg, state, Sensors(), now, en).blocked)
        state.advance(cfg, at(0, 1, day = 8), Sensors())
        assertTrue(Engine().decide(cfg, state, Sensors(), at(0, 1, day = 8), en).blocked.isEmpty())
    }

    @Test
    fun nothingLoosensWhileABlockIsActiveOrAboutToStart() {
        val cfg = Config(rules = listOf(sleep), allowed = listOf("com.example.chat"))
        // Twenty minutes before the rule starts, inside the half hour of lead.
        val soon = at(22, 40)
        assertFalse(ask(cfg, State(), soon, "rule_remove", "name" to "sleep").ok)
        assertFalse(ask(cfg, State(), soon, "set", "key" to "lead-minutes", "value" to 0).ok)
        assertFalse(ask(cfg, State(), soon, "allowed_set", "apps" to JSONArray(listOf("com.example.chat", "com.example.video"))).ok)
        // Tightening is always welcome.
        assertTrue(ask(cfg, State(), soon, "allowed_set", "apps" to JSONArray()).ok)
        assertTrue(ask(cfg, State(), soon, "set", "key" to "lead-minutes", "value" to 60).ok)
        // With time to spare the rule can go.
        assertTrue(ask(cfg, State(), at(12), "rule_remove", "name" to "sleep").config.rules.isEmpty())
    }

    @Test
    fun requestsAreCheckedBeforeTheyAreTaken() {
        val cfg = Config()
        val rule = JSONObject().put("name", "x").put("days", JSONArray(listOf("Mon"))).put("start", "09:00").put("end", "10:00")
        assertFalse(ask(cfg, State(), at(12), "rule_add", "rule" to rule).ok)
        assertTrue(ask(cfg, State(), at(12), "rule_add", "rule" to rule.put("lock", true)).ok)
        assertFalse(ask(cfg, State(), at(12), "now", "minutes" to 30, "apps" to JSONArray(listOf("com.android.dialer"))).ok)
        val started = ask(cfg, State(), at(12), "now", "minutes" to 30, "lock" to true)
        assertEquals(at(12, 30), started.config.oneoffs.single().until)
        assertTrue(Service.prune(started.config, at(12, 31)).oneoffs.isEmpty())
    }

    @Test
    fun everyLanguageSaysTheSameThings() {
        val placeholders = Regex("\\{(\\w+)}")
        val english = catalogs.getValue("en")
        for ((code, catalog) in catalogs) {
            assertEquals("keys of $code", english.keys().asSequence().toSet(), catalog.keys().asSequence().toSet())
            for (key in english.keys()) {
                val expected = placeholders.findAll(english.getString(key)).map { it.value }.toSet()
                val found = placeholders.findAll(catalog.getString(key)).map { it.value }.toSet()
                assertEquals("placeholders of $key in $code", expected, found)
            }
        }
    }

    @Test
    fun everyTextTheAppAsksForExists() {
        val used = HashSet<String>()
        val quoted = Regex("\"((?:ui|day|days|lock|uninstall|error|why|banner|effect|rule|now|reasons|set|break|allowance|allowed|language|media|appearance|notice)\\.[a-z_.]+)\"")
        (File("src/main/kotlin").walkTopDown() + File(assets, "index.html") + File(assets, "lock.html"))
            .filter { it.isFile }
            .forEach { file -> quoted.findAll(file.readText()).forEach { used += it.groupValues[1] } }
        val missing = used.filter { key -> !key.endsWith(".") && !key.endsWith(".html") && !en.has(key) }.sorted()
        assertEquals(emptyList<String>(), missing)
    }
}
