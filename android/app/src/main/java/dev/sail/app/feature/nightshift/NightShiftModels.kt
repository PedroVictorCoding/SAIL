@file:OptIn(kotlinx.serialization.ExperimentalSerializationApi::class)

package dev.sail.app.feature.nightshift

import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonClassDiscriminator

val NightShiftJson = Json {
    ignoreUnknownKeys = true
    encodeDefaults = true
}

@Serializable
enum class ErrandSource {
    @SerialName("voice") VOICE,
    @SerialName("web") WEB,
    @SerialName("phone") PHONE,
    @SerialName("telegram") TELEGRAM,
}

@Serializable
enum class ErrandSize {
    @SerialName("s") S,
    @SerialName("m") M,
    @SerialName("l") L,
}

@Serializable
data class WorkOrder(
    val title: String,
    val why: String,
    val acceptance: List<String>,
    @SerialName("target_files") val targetFiles: List<String>,
    val size: ErrandSize,
    @SerialName("notes_for_solver") val notesForSolver: String = "",
)

@Serializable
data class OwnerNote(
    val at: String,
    val text: String,
)

@Serializable
enum class RanOnKind {
    @SerialName("host") HOST,
    @SerialName("mesh") MESH,
    @SerialName("external") EXTERNAL,
}

@Serializable
data class RanOn(
    val kind: RanOnKind,
    val name: String,
    val model: String,
)

@Serializable
data class FileChange(
    val path: String,
    val added: Int,
    val removed: Int,
)

@Serializable
data class TestSummary(
    val passed: Int,
    val failed: Int,
    val ignored: Int,
    @SerialName("duration_secs") val durationSecs: Long,
    @SerialName("exit_ok") val exitOk: Boolean,
)

@Serializable
data class GateResult(
    val name: String,
    val passed: Boolean,
    val blocking: Boolean,
    val detail: String,
)

@Serializable
data class ReviewSummary(
    val passed: Boolean,
    val concerns: List<String>,
)

@Serializable
data class NightResult(
    @SerialName("job_id") val jobId: String,
    @SerialName("base_commit") val baseCommit: String,
    @SerialName("ran_on") val ranOn: RanOn,
    val files: List<FileChange>,
    val tests: TestSummary? = null,
    val gates: List<GateResult>,
    val review: ReviewSummary? = null,
    val summary: String,
    @SerialName("model_note") val modelNote: String? = null,
    @SerialName("patch_bytes") val patchBytes: Long,
    @SerialName("finished_at") val finishedAt: String,
)

@Serializable
@JsonClassDiscriminator("state")
sealed class ErrandState {
    @Serializable
    @SerialName("shaping")
    data object Shaping : ErrandState()

    @Serializable
    @SerialName("proposed")
    data class Proposed(val order: WorkOrder) : ErrandState()

    @Serializable
    @SerialName("unshapeable")
    data class Unshapeable(val reasons: List<String>) : ErrandState()

    @Serializable
    @SerialName("queued")
    data class Queued(
        val order: WorkOrder,
        @SerialName("approved_at") val approvedAt: String,
    ) : ErrandState()

    @Serializable
    @SerialName("working")
    data class Working(
        val order: WorkOrder,
        @SerialName("job_id") val jobId: String,
        val stage: String,
        @SerialName("started_at") val startedAt: String,
        @SerialName("ran_on") val ranOn: RanOn,
    ) : ErrandState()

    @Serializable
    @SerialName("ready")
    data class Ready(
        val order: WorkOrder,
        val result: NightResult,
    ) : ErrandState()

    @Serializable
    @SerialName("needs_you")
    data class NeedsYou(
        val order: WorkOrder,
        val result: NightResult,
        val question: String,
    ) : ErrandState()

    @Serializable
    @SerialName("failed")
    data class Failed(
        val order: WorkOrder,
        val result: NightResult? = null,
        val reason: String,
    ) : ErrandState()

    @Serializable
    @SerialName("shipping")
    data class Shipping(
        val order: WorkOrder,
        val result: NightResult,
    ) : ErrandState()

    @Serializable
    @SerialName("shipped")
    data class Shipped(
        val order: WorkOrder,
        val result: NightResult,
        @SerialName("pr_url") val prUrl: String,
        @SerialName("shipped_at") val shippedAt: String,
    ) : ErrandState()

    @Serializable
    @SerialName("dropped")
    data class Dropped(val at: String) : ErrandState()
}

@Serializable
data class Errand(
    val id: String,
    val ask: String,
    val source: ErrandSource,
    @SerialName("created_at") val createdAt: String,
    @SerialName("updated_at") val updatedAt: String,
    val attempts: Int,
    @SerialName("external_used") val externalUsed: Boolean,
    val notes: List<OwnerNote>,
    val state: ErrandState,
)

@Serializable
data class NightShiftSettings(
    val enabled: Boolean = false,
    @SerialName("window_start_hour") val windowStartHour: Int = 1,
    @SerialName("window_end_hour") val windowEndHour: Int = 7,
    @SerialName("max_items") val maxItems: Int = 5,
    @SerialName("include_forge_ready_issues") val includeForgeReadyIssues: Boolean = true,
    @SerialName("use_mesh") val useMesh: Boolean = true,
    @SerialName("external_solves_per_night") val externalSolvesPerNight: Int = 0,
    @SerialName("external_provider") val externalProvider: String? = null,
    @SerialName("morning_greeting") val morningGreeting: Boolean = true,
)

@Serializable
@JsonClassDiscriminator("kind")
sealed class NightItem {
    @Serializable
    @SerialName("errand")
    data class ErrandRef(val id: String) : NightItem()

    @Serializable
    @SerialName("issue")
    data class IssueRef(val number: Long) : NightItem()
}

@Serializable
data class DeferredItem(
    val item: NightItem,
    val reason: String,
)

@Serializable
data class NightPlan(
    val items: List<NightItem> = emptyList(),
    val deferred: List<DeferredItem> = emptyList(),
)

@Serializable
data class TonightInfo(
    val window: String = "01:00–07:00",
    val plan: NightPlan = NightPlan(),
)

@Serializable
enum class MorningOutcome {
    @SerialName("ready") READY,
    @SerialName("needs_you") NEEDS_YOU,
    @SerialName("failed") FAILED,
    @SerialName("not_reached") NOT_REACHED,
}

@Serializable
data class MorningItem(
    val item: NightItem,
    val title: String,
    val outcome: MorningOutcome,
    val line: String,
)

@Serializable
data class MorningReport(
    val date: String,
    val ready: Int,
    @SerialName("needs_you") val needsYou: Int,
    val failed: Int,
    @SerialName("not_reached") val notReached: Int,
    val items: List<MorningItem>,
    val spoken: String,
    val halted: String? = null,
)

@Serializable
data class MeshStatus(
    val available: Boolean,
    val name: String,
    val resting: Boolean,
)

@Serializable
data class ExternalBudget(
    val provider: String? = null,
    @SerialName("used_tonight") val usedTonight: Int = 0,
    val allowed: Int = 0,
)

@Serializable
data class NightShiftResponse(
    val settings: NightShiftSettings = NightShiftSettings(),
    val tonight: TonightInfo = TonightInfo(),
    val errands: List<Errand> = emptyList(),
    val morning: MorningReport? = null,
    val mesh: MeshStatus? = null,
    val external: ExternalBudget = ExternalBudget(),
)

@Serializable
data class WsNightShiftErrand(
    val type: String,
    val errand: Errand,
)

@Serializable
data class WsNightShiftMorning(
    val type: String,
    val report: MorningReport,
)

@Serializable
data class AskErrandRequest(
    val ask: String,
    val source: String = "phone",
)

@Serializable
data class ApproveErrandRequest(
    val order: WorkOrder? = null,
)

@Serializable
data class NoteRequest(
    val note: String,
)

private const val DRAFT_PR_PREFIX = "Draft PR opened: https://github.com/"

fun parseIssueDraftPrUrl(item: MorningItem): String? {
    if (item.item !is NightItem.IssueRef) return null
    if (!item.line.startsWith(DRAFT_PR_PREFIX)) return null
    val raw = item.line.removePrefix("Draft PR opened: ").trim()
    val uri = runCatching { java.net.URI(raw) }.getOrNull() ?: return null
    if (uri.scheme != "https" || uri.host != "github.com") return null
    return uri.toASCIIString()
}
