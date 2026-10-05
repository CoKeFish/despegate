package io.github.cokefish.despegate

import android.app.Application
import android.content.Context
import android.content.pm.ApplicationInfo
import android.net.Uri
import android.provider.OpenableColumns
import android.text.format.DateFormat
import io.github.cokefish.despegate.core.Clock
import io.github.cokefish.despegate.core.Config
import io.github.cokefish.despegate.core.DayLog
import io.github.cokefish.despegate.core.Engine
import io.github.cokefish.despegate.core.Headline
import io.github.cokefish.despegate.core.Langs
import io.github.cokefish.despegate.core.Mode
import io.github.cokefish.despegate.core.Outcome
import io.github.cokefish.despegate.core.Runtime
import io.github.cokefish.despegate.core.Sensors
import io.github.cokefish.despegate.core.Service
import io.github.cokefish.despegate.core.State
import io.github.cokefish.despegate.core.Texts
import io.github.cokefish.despegate.core.Verdict
import io.github.cokefish.despegate.core.countdown
import io.github.cokefish.despegate.platform.Apps
import io.github.cokefish.despegate.core.time
import io.github.cokefish.despegate.platform.Device
import io.github.cokefish.despegate.platform.Enforcer
import io.github.cokefish.despegate.platform.Guard
import io.github.cokefish.despegate.platform.Phone
import io.github.cokefish.despegate.platform.Watch
import org.json.JSONArray
import org.json.JSONObject
import java.io.File
import java.time.Duration
import java.time.LocalDateTime
import java.util.Locale

class App : Application() {
    lateinit var core: Core
        private set

    override fun onCreate() {
        super.onCreate()
        core = Core(this)
        Guard.start(this)
    }
}

val Context.core: Core get() = (applicationContext as App).core

/**
 * What despegate can lean on to enforce anything: owning the phone, which lets
 * the system itself hold the lock; watching through accessibility, which
 * lets despegate put things back; or nothing yet.
 */
enum class Power { OWNER, WATCH, NONE }

/** Where the pages are served from; nothing leaves the phone. */
const val ORIGIN = "https://despegate.app"

/**
 * Everything despegate knows and decides, in one place: the config and the
 * clocks on disk, the engine, and the phone it acts on. The service ticks it;
 * the pages ask it for changes.
 */
class Core(private val context: Context) {
    val device = Device(context)
    val phone = Phone(context)
    val apps = Apps(context)
    val langs = Langs(
        listOf("en", "es").map { Texts.fromJson(it, JSONObject(asset("locales/$it.json"))) },
        Locale.getDefault().language,
    )
    val mediaDir = File(context.filesDir, "media")
    private val configFile = File(context.filesDir, "config.json")
    private val stateFile = File(context.filesDir, "state.json")
    private val engine = Engine()
    private val state: State = read(stateFile)?.let(State::fromJson) ?: State()
    private var ticks = 0
    val enforcer: Enforcer by lazy { Enforcer(context, this) }

    val power: Power
        get() = when {
            device.isOwner -> Power.OWNER
            Watch.connected -> Power.WATCH
            else -> Power.NONE
        }

    init {
        // Should the record of what was suspended be lost, the phone itself still knows.
        state.suspended += device.suspended(apps.launchable().map { it.first })
    }

    var config: Config = read(configFile)?.let(Config::fromJson) ?: Config()
        private set
    var verdict = Verdict(Mode.Idle, emptySet())
        private set

    init {
        // After the config has been read, which says how times are to be written.
        setClock()
    }

    val texts: Texts get() = langs.resolve(config.language)

    /** The debug build, which a computer can take off the phone. */
    val removable: Boolean get() = context.applicationInfo.flags and ApplicationInfo.FLAG_TEST_ONLY != 0

    fun asset(path: String): String = context.assets.open(path).bufferedReader().use { it.readText() }

    /** One round: moves the clocks, decides what to enforce and remembers it. */
    @Synchronized
    fun tick(): Verdict {
        val now = LocalDateTime.now()
        val pruned = Service.prune(config, now)
        if (pruned !== config) {
            config = pruned
            write(configFile, config.toJson())
        }
        // The watch is told what comes to the front; without it, the usage records have to do.
        val front = if (Watch.connected) Watch.front else phone.foreground()
        val using = phone.inUse
        val sensors = Sensors(
            idleSecs = phone.idleSecs(),
            inUse = listOfNotNull(front).filter { using && config.allowance(it) != null },
            locked = verdict.mode is Mode.Lock,
        )
        val breakStarted = state.advance(config, now, sensors)
        // A focus session lasts until the long break, which may have just come.
        if (config.focus != null && state.focusOver(config.breaks)) {
            config = config.copy(focus = null)
            write(configFile, config.toJson())
        }
        verdict = if (power != Power.NONE) engine.decide(config, state, sensors, now, texts, apps::label) else Verdict(Mode.Idle, emptySet())

        val held = device.block(verdict.blocked, state.suspended)
        val changed = held != state.suspended
        state.suspended = held
        if (breakStarted || changed || ++ticks % SAVE_EVERY_TICKS == 0) write(stateFile, state.toJson())
        return verdict
    }

    /** Whether anything that loosens despegate is refused right now. */
    @Synchronized
    fun lockedIn(): Boolean = Service.lockedIn(config, state, LocalDateTime.now(), texts) != null

    /** The refusal to give when something would loosen despegate right now, if it would be refused. */
    @Synchronized
    fun whyLockedIn(): String? =
        Service.lockedIn(config, state, LocalDateTime.now(), texts)?.let { texts.tr("error.loosens", "why" to it) }

    /** What to say when [pkg] is sent away: until when it is blocked, or that its time for today is spent. */
    @Synchronized
    fun whyBlocked(pkg: String): String {
        val until = config.activeBlocks(LocalDateTime.now()).filter { pkg in it.apps }.maxOfOrNull { it.until }
        return when {
            until != null -> texts.tr("banner.blocked", "app" to apps.label(pkg), "until" to time(until))
            config.focus?.apps?.contains(pkg) == true && pkg !in state.exhausted(config, LocalDateTime.now()) ->
                texts.tr("banner.focus", "app" to apps.label(pkg))
            else -> texts.tr("banner.allowance_spent", "app" to apps.label(pkg))
        }
    }

    /** With nothing to lean on, nothing is being enforced. */
    @Synchronized
    fun idle() {
        verdict = Verdict(Mode.Idle, emptySet())
    }

    /** Carries out a change asked for by a page. */
    @Synchronized
    fun handle(request: JSONObject): Outcome {
        val runtime = Runtime(state, device.protectedApps(), apps::label)
        val outcome = Service.handle(request, config, LocalDateTime.now(), runtime, langs)
        if (outcome.ok) {
            if (request.optString("cmd") == "media_remove") File(mediaDir, request.optString("name")).delete()
            config = outcome.config
            write(configFile, config.toJson())
            setClock()
        }
        return outcome
    }

    /** What has been typed on the lock screen; true when it just earned the emergency pause. */
    @Synchronized
    fun typed(text: String): Boolean = engine.typed(text, config, LocalDateTime.now())

    /** Copies a photo or video in and adds it to the reasons. */
    fun importMedia(uri: Uri): Outcome {
        val t = texts
        val name = context.contentResolver.query(uri, arrayOf(OpenableColumns.DISPLAY_NAME), null, null, null)?.use {
            if (it.moveToFirst()) it.getString(0) else null
        } ?: uri.lastPathSegment ?: "media"
        val safe = name.replace(Regex("[^\\p{L}\\p{N}._ -]"), "_").takeLast(80)
        if (mediaKind(safe) == null) return Outcome(false, t.tr("error.media_unsupported"), config)
        mediaDir.mkdirs()
        var target = File(mediaDir, safe)
        var n = 2
        while (target.exists()) target = File(mediaDir, "${safe.substringBeforeLast('.')}-${n++}.${safe.substringAfterLast('.')}")
        try {
            val input = context.contentResolver.openInputStream(uri) ?: return Outcome(false, t.tr("error.media_unreadable", "error" to name), config)
            input.use { source ->
                target.outputStream().use { sink ->
                    val buffer = ByteArray(1 shl 16)
                    var total = 0L
                    while (true) {
                        val read = source.read(buffer)
                        if (read < 0) break
                        total += read
                        if (total > MAX_MEDIA_BYTES) {
                            sink.close()
                            target.delete()
                            return Outcome(false, t.tr("error.media_too_large", "max" to MAX_MEDIA_BYTES / (1024 * 1024)), config)
                        }
                        sink.write(buffer, 0, read)
                    }
                }
            }
        } catch (e: java.io.IOException) {
            target.delete()
            return Outcome(false, t.tr("error.media_unwritable", "error" to e.message), config)
        }
        return handle(JSONObject().put("cmd", "media_add").put("name", target.name))
    }

    /** The way out: every block lifted, the phone handed back. The app can then be uninstalled. */
    @Synchronized
    fun release() {
        device.release(state.suspended)
        Watch.leave()
        state.suspended = emptySet()
        verdict = Verdict(Mode.Idle, emptySet())
        write(stateFile, state.toJson())
    }

    /** What is happening now and what comes next, in words. */
    @Synchronized
    fun headline(): JSONObject = Headline.of(config, state, power != Power.NONE, engine, LocalDateTime.now(), texts, apps::label)

    /**
     * How far the forced breaks have got: the use counted since the last rest,
     * what is left before the next break, or until when one is running.
     */
    private fun breakState(now: LocalDateTime): Any {
        val policy = config.breaks ?: return JSONObject.NULL
        val until = state.onBreak(now)
        val (phase, left, total) = when {
            until != null && state.breakLong -> Triple("long", Duration.between(now, until), policy.longBreakMinutes * 60L)
            until != null -> Triple("rest", Duration.between(now, until), policy.breakMinutes * 60L)
            else -> Triple("work", state.breakDueIn(config) ?: Duration.ZERO, policy.workMinutes * 60L)
        }
        val duration = { secs: Long -> countdown(Duration.ofSeconds(secs)) }
        val week = JSONArray((6L downTo 0L).map { back ->
            val day = now.toLocalDate().minusDays(back)
            val log = state.history[day] ?: DayLog()
            JSONObject().put("day", texts.dayName(day.dayOfWeek)).put("minutes", log.workSecs / 60)
                .put("work", duration(log.workSecs)).put("breaks", log.breaks).put("today", back == 0L)
        })
        val today = state.history[now.toLocalDate()] ?: DayLog()
        return JSONObject()
            .put("phase", phase)
            .put("left", countdown(left))
            .put("left_secs", left.seconds.coerceAtLeast(0))
            .put("total_secs", total)
            .put("used", duration(state.usageSecs))
            .put("cycle", state.cycle)
            .put("cycles", policy.cycles)
            .put("has_long", policy.hasLong)
            .put("next_long", state.nextIsLong(policy))
            .put("session", config.focus?.let { f -> JSONObject().put("apps", JSONArray(f.apps)).put("labels", JSONArray(f.apps.map(apps::label))) } ?: JSONObject.NULL)
            .put("today", JSONObject().put("work", duration(today.workSecs)).put("breaks", today.breaks))
            .put("week", week)
            .put("streak", state.streak(now.toLocalDate()))
    }

    /** What changes by the second, for the page to keep up with without redrawing itself whole. */
    @Synchronized
    fun pulse(): JSONObject {
        setClock()
        return JSONObject().put("headline", headline()).put("focus", breakState(LocalDateTime.now())).put("hour12", Clock.twelveHour)
    }

    /** Times are written as the settings say: with am and pm, around the clock, or the way the phone does. */
    private fun setClock() {
        Clock.twelveHour = when (config.clock) {
            "12" -> true
            "24" -> false
            else -> !DateFormat.is24HourFormat(context)
        }
    }

    /** Everything the settings page renders, in one piece. */
    @Synchronized
    fun pageState(): JSONObject {
        val now = LocalDateTime.now()
        val t = texts
        val power = power
        val referenced = (config.rules.flatMap { it.apps } + config.oneoffs.flatMap { it.apps } + config.allowances.map { it.app } + config.allowed).distinct()
        return JSONObject()
            .put("daemon", power != Power.NONE)
            .put("power", power.name.lowercase())
            .put("owner_command", device.ownerCommand)
            .put("removable", removable)
            .put("battery_free", phone.batteryFree)
            .put("usage_access", Watch.connected || phone.hasUsageAccess)
            .put("lang", t.code)
            .put("languages", JSONArray(langs.all.map { JSONObject().put("code", it.code).put("name", it.name) }))
            .put("config", config.toJson())
            .put("active", JSONArray(config.activeBlocks(now).map { it.name }))
            .put("allowance_left", JSONObject(config.allowances.associate { it.app to countdown(state.allowanceLeft(config, it.app, now)!!) }))
            .put("media", mediaJson(config.media))
            .put("labels", JSONObject(referenced.associateWith(apps::label)))
            .put("locked_in", Service.lockedIn(config, state, now, t) ?: JSONObject.NULL)
            .put("headline", headline())
            .put("focus", breakState(now))
            .put("hour12", Clock.twelveHour)
    }

    /** The lock screen's contents. */
    fun lockView(lock: Mode.Lock): JSONObject = JSONObject()
        .put("until", io.github.cokefish.despegate.core.stamp(lock.until))
        .put("reasons", lock.reasons)
        .put("media", mediaJson(lock.media))
        .put("challenge", lock.challenge ?: JSONObject.NULL)
        .put("emergency_minutes", lock.emergencyMinutes)
        .put("allowed", JSONArray(lock.allowed.filter { apps.launchIntent(it) != null }.map { JSONObject().put("pkg", it).put("label", apps.label(it)) }))
        .put("phrase", texts.tr("uninstall.phrase"))
        .put("hour12", Clock.twelveHour)
        .put("idea", lock.idea ?: JSONObject.NULL)

    private fun mediaJson(names: List<String>): JSONArray = JSONArray(
        names.mapNotNull { name ->
            mediaKind(name)?.let { kind ->
                val address = Uri.encode(name)
                JSONObject().put("name", name).put("kind", kind).put("url", "$ORIGIN/media/$address")
                    // What the gallery shows: the photo itself, or a frame of the video.
                    .put("thumb", if (kind == "video") "$ORIGIN/thumb/$address" else "$ORIGIN/media/$address")
            }
        },
    )

    private fun read(file: File): JSONObject? = runCatching { JSONObject(file.readText()) }.getOrNull()

    /** Written whole to a second file and swapped in, so a crash never leaves half a file. */
    private fun write(file: File, json: JSONObject) {
        val fresh = File(file.parentFile, file.name + ".new")
        fresh.writeText(json.toString())
        fresh.renameTo(file)
    }

    companion object {
        private const val SAVE_EVERY_TICKS = 15
        private const val MAX_MEDIA_BYTES = 100L * 1024 * 1024

        fun mediaKind(name: String): String? = when (name.substringAfterLast('.', "").lowercase()) {
            "jpg", "jpeg", "png", "gif", "webp" -> "image"
            "mp4", "webm" -> "video"
            else -> null
        }
    }
}
