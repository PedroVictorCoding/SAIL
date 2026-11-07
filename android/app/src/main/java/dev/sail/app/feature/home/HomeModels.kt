package dev.sail.app.feature.home

import java.util.Locale
import kotlin.math.roundToInt
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.JsonElement

@Serializable
data class BriefingEnvelope(
    val briefing: ReturnBriefing? = null,
)

@Serializable
data class ReturnBriefing(
    val id: String = "",
    @SerialName("generated_at")
    val generatedAt: String = "",
    @SerialName("local_time_str")
    val localTimeStr: String = "",
    val greeting: String = "Welcome back",
    val headline: String = "Here's your return briefing.",
    @SerialName("away_duration_minutes")
    val awayDurationMinutes: Long? = null,
    @SerialName("staged_music")
    val stagedMusic: StagedMusic? = null,
    @SerialName("learning_plan")
    val learningPlan: StagedLearning? = null,
    @SerialName("events_to_watch")
    val eventsToWatch: List<BriefingEvent> = emptyList(),
    @SerialName("news_radar")
    val newsRadar: List<BriefingNewsItem> = emptyList(),
    @SerialName("project_continuity")
    val projectContinuity: ProjectContinuity? = null,
    @SerialName("system_sentinel")
    val systemSentinel: SentinelBriefing? = null,
    val dismissed: Boolean = false,
)

@Serializable
data class StagedMusic(
    val uri: String,
    val name: String,
    val by: String = "",
    val kind: String = "track",
    @SerialName("circadian_slot")
    val circadianSlot: String = "",
    val rationale: String = "",
    @SerialName("is_spotify")
    val isSpotify: Boolean = false,
    val confirmed: Boolean = false,
)

@Serializable
data class StagedLearning(
    @SerialName("session_id")
    val sessionId: String? = null,
    val topic: String,
    val rationale: String = "",
    val status: String = "",
    @SerialName("estimated_minutes")
    val estimatedMinutes: Int = 0,
    val domain: String = "",
)

@Serializable
data class BriefingEvent(
    val id: String = "",
    val title: String,
    @SerialName("due_at")
    val dueAt: String = "",
    @SerialName("due_relative")
    val dueRelative: String = "",
    val urgency: String = "normal",
    val source: String = "Pulse",
    @SerialName("action_type")
    val actionType: String = "info",
)

@Serializable
data class BriefingNewsItem(
    val id: String = "",
    val title: String,
    val source: String = "",
    val summary: String = "",
    val url: String? = null,
    @SerialName("relevant_interest")
    val relevantInterest: String = "",
)

@Serializable
data class ProjectContinuity(
    @SerialName("active_workspace")
    val activeWorkspace: String = "SAIL",
    @SerialName("forge_summary")
    val forgeSummary: String? = null,
    @SerialName("last_edited_file")
    val lastEditedFile: String? = null,
    @SerialName("suggested_action")
    val suggestedAction: String = "",
)

@Serializable
data class SentinelBriefing(
    @SerialName("host_health")
    val hostHealth: String = "",
    @SerialName("storage_status")
    val storageStatus: String = "",
    @SerialName("memory_headroom")
    val memoryHeadroom: String = "",
    @SerialName("pending_guardian_approvals")
    val pendingGuardianApprovals: Int = 0,
)

@Serializable
data class PlayMusicRequest(
    val uri: String,
)

@Serializable
data class PlayMusicResponse(
    val ok: Boolean = false,
    val error: String? = null,
)

@Serializable
data class DismissBriefingResponse(
    val dismissed: Boolean = false,
)

@Serializable
data class SpotifyStatusResponse(
    val configured: Boolean = false,
    val connected: Boolean = false,
    @SerialName("redirect_uri")
    val redirectUri: String = "",
    @SerialName("now_playing")
    val nowPlaying: SpotifyNowPlaying? = null,
    @SerialName("can_stream")
    val canStream: Boolean = false,
    val error: String? = null,
    val code: String? = null,
)

@Serializable
data class SpotifyNowPlaying(
    @SerialName("is_playing")
    val isPlaying: Boolean = false,
    val name: String = "",
    val by: String = "",
    val album: String = "",
    val image: String? = null,
    val uri: String = "",
    @SerialName("progress_ms")
    val progressMs: Long? = null,
    @SerialName("duration_ms")
    val durationMs: Long? = null,
    val device: String = "",
    val context: String = "",
)

@Serializable
data class HomeCoursesResponse(
    val courses: List<HomeCourseSummary> = emptyList(),
)

@Serializable
data class HomeCourseSummary(
    val id: String,
    val title: String,
    val domain: String = "",
    val status: String = "",
    val summary: String = "",
    val version: String? = null,
    @SerialName("lesson_count")
    val lessonCount: Int = 0,
    val minutes: Int = 0,
    val progress: HomeCourseProgress? = null,
    val next: HomeLearnNext? = null,
)

@Serializable
data class HomeCourseProgress(
    @SerialName("lessons_completed")
    val lessonsCompleted: Int = 0,
    val objectives: Int = 0,
    val provisional: Int = 0,
    val retained: Int = 0,
    val due: Int = 0,
)

@Serializable
data class HomeLearnNext(
    val kind: String = "",
    @SerialName("lesson_id")
    val lessonId: String? = null,
    val reasons: List<String> = emptyList(),
)

@Serializable
data class SystemMetrics(
    @SerialName("cpu_percent")
    val cpuPercent: Double? = null,
    @SerialName("cpu_cores")
    val cpuCores: Int? = null,
    val load1: Double? = null,
    @SerialName("ram_total_bytes")
    val ramTotalBytes: Long? = null,
    @SerialName("ram_used_bytes")
    val ramUsedBytes: Long? = null,
    @SerialName("swap_total_bytes")
    val swapTotalBytes: Long? = null,
    @SerialName("swap_used_bytes")
    val swapUsedBytes: Long? = null,
    @SerialName("io_bytes_per_sec")
    val ioBytesPerSec: Double? = null,
    val disks: List<DiskMetric> = emptyList(),
    val gpus: List<GpuMetric> = emptyList(),
    @SerialName("uptime_secs")
    val uptimeSecs: Double? = null,
    val version: String? = null,
)

@Serializable
data class DiskMetric(
    val path: String,
    @SerialName("used_bytes")
    val usedBytes: Long = 0,
    @SerialName("total_bytes")
    val totalBytes: Long = 0,
) {
    val freeBytes: Long
        get() = (totalBytes - usedBytes).coerceAtLeast(0L)
}

@Serializable
data class GpuMetric(
    val card: String = "",
    @SerialName("busy_percent")
    val busyPercent: Double? = null,
    @SerialName("vram_used_bytes")
    val vramUsedBytes: Long? = null,
    @SerialName("vram_total_bytes")
    val vramTotalBytes: Long? = null,
)

@Serializable
data class HealthResponse(
    val speech: JsonElement? = null,
    val llm: JsonElement? = null,
    @SerialName("sample_rate")
    val sampleRate: Int? = null,
)

@Serializable
data class HomeMindStatus(
    val version: Int = 1,
    @SerialName("accepting_requests")
    val acceptingRequests: Boolean = false,
    val models: List<HomeMindModel> = emptyList(),
)

@Serializable
data class HomeMindModel(
    val model: String = "",
    @SerialName("last_error")
    val lastError: String? = null,
)

private const val BYTES_PER_GIB = 1_073_741_824.0

object HomeFormatting {
    fun formatGib(bytes: Long): String {
        val gib = bytes.coerceAtLeast(0L) / BYTES_PER_GIB
        return String.format(Locale.US, "%.1f", gib)
    }

    fun formatFreeStorage(metrics: SystemMetrics?): String? {
        val disks = metrics?.disks?.filter { it.totalBytes > 0L }.orEmpty()
        if (disks.isEmpty()) return null
        val primary = disks.firstOrNull { it.path == "/" } ?: disks.first()
        return "${formatGib(primary.freeBytes)} GB free"
    }

    fun formatMemoryUsage(metrics: SystemMetrics?): String? {
        val used = metrics?.ramUsedBytes ?: return null
        val total = metrics.ramTotalBytes ?: return null
        if (total <= 0L) return null
        return "${formatGib(used)} / ${formatGib(total)} GB"
    }

    fun formatCpuPercent(cpuPercent: Double?): String? {
        val value = cpuPercent?.takeIf { it.isFinite() } ?: return null
        return "${value.roundToInt().coerceIn(0, 100)}%"
    }
}
