package dev.sail.app.feature.dock

import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.os.BatteryManager
import dev.sail.app.core.net.SailApi
import dev.sail.app.core.net.getOrNull
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import java.util.UUID

/**
 * Foreground presence heartbeat reporter for Android.
 * Sends metadata only (surface, foreground visibility, input idle seconds, charging, voice_ready)
 * every 30 s while the app is in the foreground.
 */
class Heartbeats(
    private val context: Context,
    private val api: SailApi,
    private val scope: CoroutineScope,
    private val nowMs: () -> Long = System::currentTimeMillis,
) {
    private val prefs = context.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)

    @Volatile private var activeSurface: SurfaceKind = SurfaceKind.PHONE
    @Volatile private var voiceReady: Boolean = false
    @Volatile private var foreground: Boolean = false
    @Volatile private var lastInputMs: Long = nowMs()
    private var loopJob: Job? = null

    private val mutableCharging = MutableStateFlow(readCharging())
    val charging: StateFlow<Boolean> = mutableCharging.asStateFlow()

    private val mutableOpenDockWhenCharging = MutableStateFlow(
        prefs.getBoolean(KEY_OPEN_DOCK_WHEN_CHARGING, false),
    )
    val openDockWhenCharging: StateFlow<Boolean> = mutableOpenDockWhenCharging.asStateFlow()

    private val mutableLastState = MutableStateFlow<PresenceState?>(null)
    val lastState: StateFlow<PresenceState?> = mutableLastState.asStateFlow()

    fun deviceId(surface: SurfaceKind): String {
        val key = if (surface == SurfaceKind.DOCK) KEY_DOCK_DEVICE_ID else KEY_PHONE_DEVICE_ID
        val prefix = if (surface == SurfaceKind.DOCK) "dock" else "phone"
        val existing = prefs.getString(key, null)
        if (existing != null && DEVICE_ID_REGEX.matches(existing)) return existing
        val suffix = UUID.randomUUID().toString().replace("-", "").take(8)
        val created = "$prefix-$suffix"
        prefs.edit().putString(key, created).apply()
        return created
    }

    fun setOpenDockWhenCharging(enabled: Boolean) {
        prefs.edit().putBoolean(KEY_OPEN_DOCK_WHEN_CHARGING, enabled).apply()
        mutableOpenDockWhenCharging.value = enabled
    }

    fun setSurface(surface: SurfaceKind, voiceReady: Boolean = this.voiceReady) {
        val changed = activeSurface != surface || this.voiceReady != voiceReady
        activeSurface = surface
        this.voiceReady = voiceReady
        if (changed && foreground) {
            scope.launch { sendOnce() }
        }
    }

    fun noteTouch() {
        lastInputMs = nowMs()
    }

    fun noteAddressedTurn() {
        lastInputMs = nowMs()
    }

    fun start() {
        foreground = true
        mutableCharging.value = readCharging()
        if (loopJob?.isActive == true) return
        loopJob = scope.launch {
            while (isActive && foreground) {
                sendOnce()
                delay(HEARTBEAT_INTERVAL_MS)
            }
        }
    }

    fun stop() {
        foreground = false
        loopJob?.cancel()
        loopJob = null
    }

    suspend fun sendOnce() {
        val isCharging = readCharging()
        mutableCharging.value = isCharging
        val idleSecs = ((nowMs() - lastInputMs) / 1000L).toInt().coerceIn(0, MAX_IDLE_SECS)
        val payload = buildHeartbeatPayload(
            deviceId = deviceId(activeSurface),
            surface = activeSurface,
            visible = foreground,
            inputIdleSecs = idleSecs,
            charging = isCharging,
            voiceReady = voiceReady,
        )
        api.postHeartbeat(payload).getOrNull()?.let { mutableLastState.value = it.state }
    }

    private fun readCharging(): Boolean {
        val intent = context.registerReceiver(null, IntentFilter(Intent.ACTION_BATTERY_CHANGED))
            ?: return false
        val plugged = intent.getIntExtra(BatteryManager.EXTRA_PLUGGED, 0)
        if (plugged != 0) return true
        val status = intent.getIntExtra(BatteryManager.EXTRA_STATUS, -1)
        return status == BatteryManager.BATTERY_STATUS_CHARGING ||
            status == BatteryManager.BATTERY_STATUS_FULL
    }

    companion object {
        private const val PREFS_NAME = "sail_presence"
        private const val KEY_DOCK_DEVICE_ID = "dock_device_id"
        private const val KEY_PHONE_DEVICE_ID = "phone_device_id"
        private const val KEY_OPEN_DOCK_WHEN_CHARGING = "open_dock_when_charging"
        private const val HEARTBEAT_INTERVAL_MS = 30_000L
        private const val MAX_IDLE_SECS = 86_400
        private val DEVICE_ID_REGEX = Regex("^[A-Za-z0-9_-]{1,64}$")

        fun buildHeartbeatPayload(
            deviceId: String,
            surface: SurfaceKind,
            visible: Boolean,
            inputIdleSecs: Int,
            charging: Boolean?,
            voiceReady: Boolean,
        ): HeartbeatRequest = HeartbeatRequest(
            deviceId = deviceId,
            surface = surface,
            visible = visible,
            inputIdleSecs = inputIdleSecs.coerceIn(0, MAX_IDLE_SECS),
            charging = charging,
            voiceReady = voiceReady,
        )
    }
}
