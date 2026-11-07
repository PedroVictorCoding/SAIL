package dev.sail.app.feature.workspace

import dev.sail.app.core.net.ApiResult
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonNull
import java.time.Instant
import java.time.ZoneId
import java.time.format.DateTimeFormatter
import java.util.Locale
import kotlin.math.roundToInt

@Serializable
data class Health(
    val speech: SpeechHealth? = null,
    val llm: LlmHealth? = null,
    @SerialName("sample_rate") val sampleRate: Int? = null,
)

@Serializable
data class SpeechHealth(
    val vad: ServiceHealth? = null,
    val stt: ServiceHealth? = null,
    val tts: ServiceHealth? = null,
) {
    val readyCount: Int
        get() = listOfNotNull(vad, stt, tts).count { it.ready }
}

@Serializable
data class ServiceHealth(
    val ready: Boolean = false,
    val error: String? = null,
)

@Serializable
data class LlmHealth(
    val ready: Boolean = false,
    val model: String? = null,
)

@Serializable
data class MindStatus(
    val version: Int = 0,
    @SerialName("accepting_requests") val acceptingRequests: Boolean = false,
    val models: List<ModelStatus> = emptyList(),
    val metrics: InferenceMetrics? = null,
    val limits: ResourceLimits? = null,
    val resources: ResourceStatus? = null,
)

@Serializable
data class ModelStatus(
    val version: Int = 0,
    val state: String = "uninitialized",
    val model: String = "",
    @SerialName("last_error") val lastError: JsonElement? = null,
) {
    val hasError: Boolean
        get() = lastError != null && lastError !is JsonNull
}

@Serializable
data class InferenceMetrics(
    val submitted: Long = 0,
    val completed: Long = 0,
    val failed: Long = 0,
    val cancelled: Long = 0,
    val rejected: Long = 0,
    val active: Int = 0,
    val queued: Int = 0,
    @SerialName("total_inference_ms") val totalInferenceMs: Long = 0,
    @SerialName("last_first_token_ms") val lastFirstTokenMs: Long? = null,
)

@Serializable
data class ResourceLimits(
    @SerialName("max_parallel") val maxParallel: Int = 0,
    @SerialName("max_queued") val maxQueued: Int = 0,
    @SerialName("max_output_tokens") val maxOutputTokens: Int = 0,
    @SerialName("queue_timeout_ms") val queueTimeoutMs: Long = 0,
    @SerialName("inference_timeout_ms") val inferenceTimeoutMs: Long = 0,
)

@Serializable
data class ResourceStatus(
    @SerialName("voice_turns") val voiceTurns: Int = 0,
    @SerialName("admitted_inference") val admittedInference: Int = 0,
    @SerialName("speech_workers") val speechWorkers: Int = 0,
    @SerialName("ram_headroom_bytes") val ramHeadroomBytes: Long = 0,
    @SerialName("resident_heavy_models") val residentHeavyModels: Int = 0,
    @SerialName("active_browsers") val activeBrowsers: Int = 0,
    @SerialName("active_builds") val activeBuilds: Int = 0,
    @SerialName("pending_ram_bytes") val pendingRamBytes: Long = 0,
    @SerialName("reserved_cpu_slots") val reservedCpuSlots: Int = 0,
    val rejected: Long = 0,
    val preemptions: Long = 0,
)

@Serializable
data class ModelCatalog(
    val models: List<CatalogModel> = emptyList(),
    val running: String? = null,
    @SerialName("running_file") val runningFile: String? = null,
    val default: String? = null,
    @SerialName("restart_required") val restartRequired: Boolean = false,
    @SerialName("env_override") val envOverride: Boolean = false,
)

@Serializable
data class CatalogModel(
    val id: String = "",
    val name: String = "",
    val summary: String = "",
    @SerialName("size_gb") val sizeGb: Double? = null,
    @SerialName("active_params_b") val activeParamsB: Double? = null,
    @SerialName("total_params_b") val totalParamsB: Double? = null,
    val vision: Boolean = false,
    val source: String? = null,
    val installed: Boolean = false,
    val running: Boolean = false,
    val default: Boolean = false,
)

@Serializable
data class PlansResponse(val plans: List<PlanView> = emptyList())

@Serializable
data class PlanView(
    val plan: Plan = Plan(),
    @SerialName("task_state") val taskState: String? = null,
    @SerialName("task_reason") val taskReason: String? = null,
)

@Serializable
data class Plan(
    val id: String = "",
    val goal: String = "",
    val state: String = "",
    val revision: Int = 0,
)

@Serializable
data class SystemMetrics(
    @SerialName("cpu_percent") val cpuPercent: Double? = null,
    @SerialName("cpu_cores") val cpuCores: Int? = null,
    val load1: Double? = null,
    @SerialName("ram_total_bytes") val ramTotalBytes: Long? = null,
    @SerialName("ram_used_bytes") val ramUsedBytes: Long? = null,
    @SerialName("swap_total_bytes") val swapTotalBytes: Long? = null,
    @SerialName("swap_used_bytes") val swapUsedBytes: Long? = null,
    @SerialName("io_bytes_per_sec") val ioBytesPerSec: Double? = null,
    val disks: List<DiskUsage> = emptyList(),
    val gpus: List<GpuUsage> = emptyList(),
    @SerialName("uptime_secs") val uptimeSecs: Double? = null,
    val version: String? = null,
)

@Serializable
data class DiskUsage(
    val path: String = "",
    @SerialName("used_bytes") val usedBytes: Long = 0,
    @SerialName("total_bytes") val totalBytes: Long = 0,
) {
    val usedFraction: Float
        get() = if (totalBytes <= 0) 0f else (usedBytes.toDouble() / totalBytes).toFloat().coerceIn(0f, 1f)
}

@Serializable
data class GpuUsage(
    val card: String = "",
    @SerialName("busy_percent") val busyPercent: Double? = null,
    @SerialName("vram_used_bytes") val vramUsedBytes: Long? = null,
    @SerialName("vram_total_bytes") val vramTotalBytes: Long? = null,
)

data class WorkspaceData(
    val health: ApiResult<Health>? = null,
    val mind: ApiResult<MindStatus>? = null,
    val metrics: ApiResult<SystemMetrics>? = null,
    val catalog: ApiResult<ModelCatalog>? = null,
    val plans: ApiResult<PlansResponse>? = null,
    val updatedAt: Long? = null,
    val loading: Boolean = false,
) {
    val isEmpty: Boolean
        get() = health == null && mind == null && metrics == null && catalog == null && plans == null
}

enum class LiveState(val label: String) {
    Active("active"),
    Idle("idle"),
    Attention("attention"),
    Offline("offline"),
    NoStatus("no status"),
}

data class SubsystemStatus(
    val id: String,
    val name: String,
    val subtitle: String,
    val state: LiveState,
    val detail: String,
)

data class TopologyLink(
    val from: String,
    val to: String,
    val state: LiveState,
    val detail: String,
)

val WORKSPACE_LINKS: List<Pair<String, String>> = listOf(
    "core" to "mind",
    "core" to "system",
    "core" to "memory",
    "core" to "flow",
    "core" to "forge",
    "core" to "arena",
    "mind" to "memory",
    "memory" to "arena",
)

fun buildSubsystems(data: WorkspaceData): List<SubsystemStatus> = listOf(
    coreStatus(data),
    mindStatus(data),
    systemStatus(data),
    SubsystemStatus(
        id = "memory",
        name = "Memory",
        subtitle = "Vector · Knowledge",
        state = LiveState.NoStatus,
        detail = "This build has no live status endpoint for Memory.",
    ),
    SubsystemStatus(
        id = "flow",
        name = "Flow",
        subtitle = "Automation · Workflows",
        state = LiveState.NoStatus,
        detail = "This build has no live status endpoint for Flow.",
    ),
    forgeStatus(data),
    SubsystemStatus(
        id = "arena",
        name = "Arena",
        subtitle = "Learn · Eval",
        state = LiveState.NoStatus,
        detail = "This build has no live status endpoint for Arena.",
    ),
)

fun buildTopology(subsystems: List<SubsystemStatus>): List<TopologyLink> {
    val byId = subsystems.associateBy { it.id }
    return WORKSPACE_LINKS.mapNotNull { (from, to) ->
        val a = byId[from] ?: return@mapNotNull null
        val b = byId[to] ?: return@mapNotNull null
        TopologyLink(
            from = from,
            to = to,
            state = combine(a.state, b.state),
            detail = "${a.state.label} · ${b.state.label}",
        )
    }
}

fun workspaceError(data: WorkspaceData): String? = listOf(
    data.health,
    data.mind,
    data.metrics,
    data.catalog,
    data.plans,
).filterIsInstance<ApiResult.Err>().firstOrNull()?.message

fun formatBytes(bytes: Long?): String {
    if (bytes == null) return "—"
    if (bytes <= 0) return "0 GB"
    val tib = bytes / 1099511627776.0
    if (tib >= 1.0) return String.format(Locale.US, "%.1f TB", tib)
    val gib = bytes / 1073741824.0
    if (gib >= 1.0) return String.format(Locale.US, "%.1f GB", gib)
    val mib = bytes / 1048576.0
    if (mib >= 1.0) return String.format(Locale.US, "%.0f MB", mib)
    return String.format(Locale.US, "%.0f KB", bytes / 1024.0)
}

fun formatPercent(value: Double?): String =
    value?.takeIf { it.isFinite() }?.let { "${it.roundToInt()}%" } ?: "—"

fun formatDecimal(value: Double?): String =
    value?.takeIf { it.isFinite() }?.let { String.format(Locale.US, "%.2f", it) } ?: "—"

fun formatUptime(seconds: Double?): String {
    val total = seconds?.toLong() ?: return "—"
    val days = total / 86_400
    val hours = (total % 86_400) / 3_600
    val minutes = (total % 3_600) / 60
    return when {
        days > 0 -> "${days}d ${hours}h"
        hours > 0 -> "${hours}h ${minutes}m"
        else -> "${minutes}m"
    }
}

fun shortModel(model: String?): String? = model
    ?.substringAfterLast('/')
    ?.substringBeforeLast('.')
    ?.takeIf { it.isNotBlank() }

fun humanPlanState(state: String): String =
    state.replace('_', ' ').replaceFirstChar { it.uppercaseChar().toString() }

fun clockLabel(epochMs: Long): String = DateTimeFormatter
    .ofPattern("HH:mm:ss")
    .withZone(ZoneId.systemDefault())
    .format(Instant.ofEpochMilli(epochMs))

fun <T> ApiResult<T>?.valueOrNull(): T? = (this as? ApiResult.Ok<T>)?.data

private fun coreStatus(data: WorkspaceData): SubsystemStatus {
    val health = data.health.valueOrNull()
    val metrics = data.metrics.valueOrNull()
    val detail = when {
        health != null -> {
            val speech = health.speech
            val ready = speech?.readyCount ?: 0
            val llm = if (health.llm?.ready == true) "LLM ready" else "LLM not ready"
            val model = shortModel(health.llm?.model)
            "Speech $ready/3 · $llm" + (model?.let { " · $it" } ?: "")
        }
        metrics != null -> "CPU ${formatPercent(metrics.cpuPercent)} · load ${formatDecimal(metrics.load1)}"
        else -> errorText(data.health, data.metrics)
    }
    val state = when {
        health == null && metrics == null -> LiveState.Offline
        health?.llm?.ready == false -> LiveState.Attention
        else -> LiveState.Active
    }
    return SubsystemStatus(
        id = "core",
        name = "SAIL Core",
        subtitle = "Orchestrator · Router · Policy",
        state = state,
        detail = detail,
    )
}

private fun mindStatus(data: WorkspaceData): SubsystemStatus {
    val mind = data.mind.valueOrNull()
    if (mind == null) {
        return SubsystemStatus(
            id = "mind",
            name = "Mind",
            subtitle = "Model Hub",
            state = LiveState.Offline,
            detail = errorText(data.mind),
        )
    }
    val ready = mind.models.count { it.state.equals("ready", ignoreCase = true) }
    val attention = mind.models.any { it.hasError }
    val state = when {
        attention -> LiveState.Attention
        ready > 0 || mind.acceptingRequests -> LiveState.Active
        else -> LiveState.Idle
    }
    val catalog = data.catalog.valueOrNull()
    val running = shortModel(catalog?.running) ?: catalog?.runningFile
    val detail = buildString {
        append("$ready/${mind.models.size} models ready")
        if (running != null) append(" · running $running")
        if (!mind.acceptingRequests) append(" · not accepting")
    }
    return SubsystemStatus(
        id = "mind",
        name = "Mind",
        subtitle = "Model Hub",
        state = state,
        detail = detail,
    )
}

private fun systemStatus(data: WorkspaceData): SubsystemStatus {
    val metrics = data.metrics.valueOrNull()
    return SubsystemStatus(
        id = "system",
        name = "System",
        subtitle = "OS · Resources",
        state = if (metrics != null) LiveState.Active else LiveState.Offline,
        detail = metrics?.let {
            "RAM ${formatBytes(it.ramUsedBytes)} of ${formatBytes(it.ramTotalBytes)}"
        } ?: errorText(data.metrics),
    )
}

private fun forgeStatus(data: WorkspaceData): SubsystemStatus {
    val response = data.plans.valueOrNull()
    val plans = response?.plans.orEmpty()
    val running = plans.count { it.plan.state == "running" }
    val awaiting = plans.count { it.plan.state == "awaiting_approval" }
    val state = when {
        response == null -> LiveState.Offline
        running > 0 -> LiveState.Active
        awaiting > 0 -> LiveState.Attention
        else -> LiveState.Idle
    }
    val detail = when {
        response == null -> errorText(data.plans)
        plans.isEmpty() -> "No plans yet"
        else -> "$running running · $awaiting awaiting · ${plans.size} total"
    }
    return SubsystemStatus(
        id = "forge",
        name = "Forge",
        subtitle = "Build · Agents · Tools",
        state = state,
        detail = detail,
    )
}

private fun combine(a: LiveState, b: LiveState): LiveState = when {
    a == LiveState.NoStatus || b == LiveState.NoStatus -> LiveState.NoStatus
    a == LiveState.Offline || b == LiveState.Offline -> LiveState.Offline
    a == LiveState.Attention || b == LiveState.Attention -> LiveState.Attention
    a == LiveState.Idle || b == LiveState.Idle -> LiveState.Idle
    else -> LiveState.Active
}

private fun errorText(vararg results: ApiResult<*>?): String =
    results.filterIsInstance<ApiResult.Err>().firstOrNull()?.message ?: "SAIL did not answer"
