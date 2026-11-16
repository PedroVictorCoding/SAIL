package dev.sail.app.feature.dock

import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable

@Serializable
enum class SurfaceKind {
    @SerialName("web") WEB,
    @SerialName("dock") DOCK,
    @SerialName("phone") PHONE,
}

@Serializable
enum class Place {
    @SerialName("desk") DESK,
    @SerialName("dock") DOCK,
    @SerialName("phone") PHONE,
    @SerialName("away") AWAY,
    @SerialName("asleep") ASLEEP,
    @SerialName("unknown") UNKNOWN,
}

@Serializable
data class HeartbeatRequest(
    @SerialName("device_id") val deviceId: String,
    val surface: SurfaceKind,
    val visible: Boolean,
    @SerialName("input_idle_secs") val inputIdleSecs: Int,
    val charging: Boolean? = null,
    @SerialName("voice_ready") val voiceReady: Boolean = false,
)

@Serializable
data class SurfaceStatus(
    @SerialName("device_id") val deviceId: String = "",
    val surface: SurfaceKind = SurfaceKind.PHONE,
    val visible: Boolean = false,
    @SerialName("input_idle_secs") val inputIdleSecs: Int = 0,
    @SerialName("last_seen") val lastSeen: String = "",
    @SerialName("voice_ready") val voiceReady: Boolean = false,
    @SerialName("can_speak") val canSpeak: Boolean = false,
)

@Serializable
data class PresenceState(
    val place: Place = Place.UNKNOWN,
    val since: String = "",
    @SerialName("active_device") val activeDevice: String? = null,
    val surfaces: List<SurfaceStatus> = emptyList(),
)

@Serializable
data class HeartbeatResponse(
    val state: PresenceState = PresenceState(),
)

@Serializable
data class PresenceSettings(
    @SerialName("away_after_secs") val awayAfterSecs: Long = 600,
    @SerialName("greet_on_arrival") val greetOnArrival: Boolean = true,
    @SerialName("greet_after_away_secs") val greetAfterAwaySecs: Long = 2700,
    @SerialName("speak_announcements") val speakAnnouncements: Boolean = true,
    @SerialName("speak_on") val speakOn: List<SurfaceKind> = listOf(SurfaceKind.WEB, SurfaceKind.DOCK),
    @SerialName("max_spoken_per_hour") val maxSpokenPerHour: Int = 3,
    @SerialName("telegram_when_away") val telegramWhenAway: Boolean = false,
)

@Serializable
data class FeedbackRequest(
    val feedback: String,
)

@Serializable
data class PulseReminderItem(
    val id: String = "",
    val text: String = "",
    val status: String = "",
    @SerialName("due_at") val dueAt: String? = null,
)

@Serializable
data class PulseRemindersResponse(
    val reminders: List<PulseReminderItem> = emptyList(),
)

data class DockAnnouncement(
    val itemId: String,
    val title: String,
    val body: String,
    val source: String,
    val kind: String,
    val link: String?,
    val speak: Boolean,
    val reasons: List<String>,
)

data class GreetingItem(
    val id: String,
    val title: String,
)

data class DockGreeting(
    val spoken: String,
    val items: List<GreetingItem>,
    val awayForSecs: Long,
)

data class DockUiState(
    val place: Place = Place.UNKNOWN,
    val charging: Boolean = false,
    val handsFreeState: String = "off",
    val listening: Boolean = true,
    val muted: Boolean = false,
    val orbState: String = "idle",
    val greeting: DockGreeting? = null,
    val announcements: List<DockAnnouncement> = emptyList(),
    val nextReminder: PulseReminderItem? = null,
    val runningTaskGoal: String? = null,
    val error: String? = null,
)
