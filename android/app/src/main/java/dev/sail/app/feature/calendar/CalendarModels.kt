@file:OptIn(kotlinx.serialization.ExperimentalSerializationApi::class)

package dev.sail.app.feature.calendar

import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonClassDiscriminator

val CalendarJson = Json {
    ignoreUnknownKeys = true
    encodeDefaults = true
}

@Serializable
enum class SourceKind {
    @SerialName("local") LOCAL,
    @SerialName("ics_feed") ICS_FEED,
}

@Serializable
enum class Freshness {
    @SerialName("fresh") FRESH,
    @SerialName("stale") STALE,
    @SerialName("failed") FAILED,
    @SerialName("never") NEVER,
}

@Serializable
data class Source(
    val id: String,
    val kind: SourceKind,
    val name: String,
    val color: String,
    val visible: Boolean,
    val writable: Boolean,
    @SerialName("url_hint") val urlHint: String? = null,
    @SerialName("refresh_minutes") val refreshMinutes: Int = 30,
    @SerialName("event_count") val eventCount: Int = 0,
    @SerialName("last_sync_at") val lastSyncAt: String? = null,
    @SerialName("last_error") val lastError: String? = null,
    val freshness: Freshness = Freshness.FRESH,
    val truncated: Boolean = false,
)

@Serializable
data class SourcesListResponse(
    val sources: List<Source> = emptyList(),
)

@Serializable
enum class EventStatus {
    @SerialName("confirmed") CONFIRMED,
    @SerialName("tentative") TENTATIVE,
    @SerialName("cancelled") CANCELLED,
}

@Serializable
@JsonClassDiscriminator("type")
sealed class EventTime {
    @Serializable
    @SerialName("timed")
    data class Timed(
        val start: String,
        val end: String,
        val tz: String,
    ) : EventTime()

    @Serializable
    @SerialName("all_day")
    data class AllDay(
        @SerialName("start_date") val startDate: String,
        @SerialName("end_date") val endDate: String,
    ) : EventTime()
}

@Serializable
enum class RepeatPreset {
    @SerialName("none") NONE,
    @SerialName("daily") DAILY,
    @SerialName("weekdays") WEEKDAYS,
    @SerialName("weekly") WEEKLY,
    @SerialName("monthly") MONTHLY,
    @SerialName("yearly") YEARLY,
}

@Serializable
data class Event(
    val id: String,
    @SerialName("source_id") val sourceId: String,
    val uid: String,
    @SerialName("recurrence_id") val recurrenceId: String? = null,
    val title: String,
    val time: EventTime,
    val location: String? = null,
    val notes: String? = null,
    val status: EventStatus = EventStatus.CONFIRMED,
    val rrule: String? = null,
    val exdates: List<String> = emptyList(),
    val repeat: RepeatPreset = RepeatPreset.NONE,
    @SerialName("repeat_until") val repeatUntil: String? = null,
    val prepare: Boolean = false,
    val writable: Boolean = true,
    @SerialName("updated_at") val updatedAt: String,
)

@Serializable
data class Occurrence(
    val key: String,
    @SerialName("event_id") val eventId: String,
    @SerialName("source_id") val sourceId: String,
    val title: String,
    val time: EventTime,
    val location: String? = null,
    val status: EventStatus = EventStatus.CONFIRMED,
    val recurring: Boolean = false,
    val prepare: Boolean = false,
    val writable: Boolean = true,
)

@Serializable
data class EventDraft(
    val title: String,
    val time: EventTime,
    val location: String? = null,
    val notes: String? = null,
    val repeat: RepeatPreset = RepeatPreset.NONE,
    @SerialName("repeat_until") val repeatUntil: String? = null,
)

@Serializable
data class EventPatch(
    val title: String? = null,
    val time: EventTime? = null,
    val location: String? = null,
    val notes: String? = null,
    val repeat: RepeatPreset? = null,
    val prepare: Boolean? = null,
)

@Serializable
data class NewFeed(
    val name: String,
    val url: String,
    val color: String? = null,
)

@Serializable
data class SourcePatch(
    val name: String? = null,
    val color: String? = null,
    val visible: Boolean? = null,
    @SerialName("refresh_minutes") val refreshMinutes: Int? = null,
)

@Serializable
data class SourceFreshness(
    val id: String,
    val freshness: Freshness,
    @SerialName("last_sync_at") val lastSyncAt: String? = null,
)

@Serializable
data class EventsWindow(
    val occurrences: List<Occurrence> = emptyList(),
    val truncated: Boolean = false,
    val sources: List<SourceFreshness> = emptyList(),
    val revision: Long = 0,
)

@Serializable
data class ImportCandidate(
    val uid: String,
    val title: String,
    val time: EventTime,
    val recurring: Boolean = false,
    val duplicate: Boolean = false,
)

@Serializable
data class SkipReason(
    val reason: String,
    val count: Int,
)

@Serializable
data class ImportPreview(
    @SerialName("preview_id") val previewId: String,
    val events: List<ImportCandidate> = emptyList(),
    val skipped: List<SkipReason> = emptyList(),
    val warnings: List<String> = emptyList(),
)

@Serializable
data class ImportResult(
    val imported: Int,
    val duplicates: Int,
)

@Serializable
data class Preferences(
    @SerialName("time_zone") val timeZone: String = "Europe/Lisbon",
    @SerialName("first_weekday") val firstWeekday: String = "monday",
    @SerialName("default_view") val defaultView: String = "agenda",
    @SerialName("day_start_hour") val dayStartHour: Int = 7,
    @SerialName("day_end_hour") val dayEndHour: Int = 23,
)

@Serializable
data class WsCalendarChanged(
    val type: String,
    val revision: Long,
    @SerialName("source_id") val sourceId: String? = null,
)

@Serializable
data class CalendarErrorDetail(
    val code: String,
    val message: String,
)

@Serializable
data class CalendarErrorBody(
    val error: CalendarErrorDetail,
)

fun formatOccurrenceTime(occ: Occurrence): String = when (val t = occ.time) {
    is EventTime.AllDay -> "All day (${t.startDate})"
    is EventTime.Timed -> "${t.start.drop(11).take(5)}–${t.end.drop(11).take(5)}"
}

fun nextUpOccurrences(occurrences: List<Occurrence>, limit: Int = 3): List<Occurrence> =
    occurrences
        .filter { it.status != EventStatus.CANCELLED }
        .take(limit)
