package dev.sail.app.feature.dialogue

import kotlinx.serialization.SerializationException
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.booleanOrNull
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.put
import kotlinx.serialization.json.putJsonObject

/**
 * The /ws/voice protocol as the web client (voice.js) speaks it. Server messages are JSON
 * objects with a "type"; client messages are JSON objects with an "action", plus binary
 * frames of 16 kHz mono little-endian PCM16 microphone audio.
 */
sealed interface VoiceEvent {
    data class Session(val id: String) : VoiceEvent
    data class Transcript(val text: String, val final: Boolean) : VoiceEvent
    data object TurnCommitted : VoiceEvent
    data class TurnStarted(val transcript: String) : VoiceEvent
    data class ResponseDelta(val text: String) : VoiceEvent
    data class AckStarted(val text: String) : VoiceEvent
    data object ResponseFinished : VoiceEvent
    data class AudioChunk(val wavBase64: String) : VoiceEvent
    data object TurnFinished : VoiceEvent
    data class Interruption(val state: String) : VoiceEvent
    data class ToolActivity(val tool: String, val active: Boolean) : VoiceEvent
    data class ToolResult(val tool: String, val success: Boolean, val error: String?) : VoiceEvent
    data class Canvas(val type: String, val content: String) : VoiceEvent
    data class ApprovalRequired(val sessionId: String, val tool: String?, val risk: String?, val rationale: String?) : VoiceEvent
    data class ApprovalResolved(val sessionId: String) : VoiceEvent
    data class SnapshotRequest(val requestId: String, val source: String, val question: String?) : VoiceEvent
    data class CaptureLimit(val message: String) : VoiceEvent
    data class Control(val muted: Boolean) : VoiceEvent
    data class Error(val message: String) : VoiceEvent
    data object Ignored : VoiceEvent
}

object VoiceProtocol {
    private val json = Json { ignoreUnknownKeys = true }
    private val finishedToolStates = setOf("completed", "failed", "cancelled")

    fun parse(text: String): VoiceEvent {
        val obj = try {
            json.parseToJsonElement(text) as? JsonObject
        } catch (error: SerializationException) {
            null
        } ?: return VoiceEvent.Error("SAIL sent a message this app can't read")
        return parse(obj)
    }

    fun parse(m: JsonObject): VoiceEvent = when (m.str("type")) {
        "session" -> VoiceEvent.Session(m.str("session_id").orEmpty())
        "transcript" -> VoiceEvent.Transcript(m.str("text").orEmpty(), m.bool("is_final") == true)
        "turn_committed" -> VoiceEvent.TurnCommitted
        "turn_started" -> VoiceEvent.TurnStarted(m.str("transcript").orEmpty())
        "response_delta" -> VoiceEvent.ResponseDelta(m.str("text").orEmpty())
        "ack_started" -> VoiceEvent.AckStarted(m.str("text").orEmpty())
        "response_finished" -> VoiceEvent.ResponseFinished
        "audio_chunk" -> m.str("audio_base64")?.let { VoiceEvent.AudioChunk(it) } ?: VoiceEvent.Ignored
        "turn_finished" -> VoiceEvent.TurnFinished
        "interruption" -> VoiceEvent.Interruption(m.str("state") ?: "interrupted")
        "tool_planned", "tool_started" -> VoiceEvent.ToolActivity(tool(m["tool"]), true)
        "tool_completed" -> VoiceEvent.ToolActivity(tool(m["tool"]), false)
        "tool_event" -> toolEvent(m.obj("event"))
        "tool_result" -> toolResult(m.obj("result"))
        "canvas" -> canvas(m.obj("artifact"))
        "approval_required" -> m.str("session_id")?.let {
            VoiceEvent.ApprovalRequired(it, m.str("tool"), m.str("risk_level"), m.str("rationale"))
        } ?: VoiceEvent.Ignored
        "approval_resolved" -> m.str("session_id")?.let { VoiceEvent.ApprovalResolved(it) } ?: VoiceEvent.Ignored
        "snapshot_request" -> m.str("request_id")?.let {
            VoiceEvent.SnapshotRequest(it, m.str("source") ?: "camera", m.str("question"))
        } ?: VoiceEvent.Ignored
        "capture_limit" -> VoiceEvent.CaptureLimit(m.str("message") ?: "Recording limit reached")
        "control" -> VoiceEvent.Control(m.obj("control")?.bool("is_muted") == true)
        "error" -> VoiceEvent.Error(m.str("message") ?: "SAIL reported an error")
        else -> VoiceEvent.Ignored
    }

    private fun toolEvent(event: JsonObject?): VoiceEvent {
        if (event == null) return VoiceEvent.Ignored
        val name = tool(event["tool"])
        val approval = event.str("approval_session")
        if (approval != null) {
            return VoiceEvent.ApprovalRequired(
                approval,
                name,
                null,
                "Review the exact operation with your owner credentials before continuing.",
            )
        }
        return VoiceEvent.ToolActivity(name, event.str("state") !in finishedToolStates)
    }

    private fun toolResult(result: JsonObject?): VoiceEvent {
        if (result == null) return VoiceEvent.Ignored
        val output = result["output"] as? JsonObject
        return VoiceEvent.ToolResult(
            tool(result["tool"]),
            result.bool("success") == true,
            output?.str("error"),
        )
    }

    private fun canvas(artifact: JsonObject?): VoiceEvent {
        val type = artifact?.str("artifact_type") ?: return VoiceEvent.Ignored
        val content = artifact.str("content") ?: return VoiceEvent.Ignored
        return VoiceEvent.Canvas(type, content)
    }

    /** Tool names arrive as "fs_read" or "fs:read"; the UI uses the colon form. */
    fun tool(value: JsonElement?): String =
        ((value as? JsonPrimitive)?.contentOrNull ?: "tool").replace('_', ':')

    private fun JsonObject.str(key: String): String? =
        (this[key] as? JsonPrimitive)?.takeIf { it !is JsonNull }?.contentOrNull

    private fun JsonObject.bool(key: String): Boolean? = (this[key] as? JsonPrimitive)?.booleanOrNull

    private fun JsonObject.obj(key: String): JsonObject? = this[key] as? JsonObject

    // Client actions.

    fun text(text: String, jpegBase64: String? = null): String = buildJsonObject {
        put("action", "text")
        put("text", text)
        if (jpegBase64 != null) {
            putJsonObject("vision") {
                put("format", "image/jpeg")
                put("data_base64", jpegBase64)
            }
        }
    }.toString()

    fun snapshot(requestId: String, jpegBase64: String): String = buildJsonObject {
        put("action", "snapshot")
        put("request_id", requestId)
        putJsonObject("image") {
            put("format", "image/jpeg")
            put("data_base64", jpegBase64)
        }
    }.toString()

    fun snapshotDeclined(requestId: String): String = buildJsonObject {
        put("action", "snapshot")
        put("request_id", requestId)
        put("error", "The user declined the capture.")
    }.toString()

    /** interrupt, end_turn, speech_cancel, playback_finished, toggle_mute, reset. */
    fun action(name: String): String = buildJsonObject { put("action", name) }.toString()

    /** Friendly label for the activity chip, matching voice.js. */
    fun toolLabel(tool: String): String = when (tool) {
        "os:status" -> "Checking system"
        "proc:list" -> "Checking processes"
        "fs:query" -> "Looking in workspace"
        "fs:read" -> "Reading workspace file"
        "fs:write" -> "Writing workspace file"
        "vision:frame" -> "Analyzing image"
        "visual:snapshot" -> "Looking"
        "forge:enqueue" -> "Dispatching Forge task"
        "forge:status" -> "Checking Forge job"
        "forge:list" -> "Listing Forge tasks"
        "github:issues" -> "Checking GitHub repo"
        "web:read" -> "Reading web page"
        "web:speak" -> "Reading web page aloud"
        else -> "Using a local tool"
    }
}
