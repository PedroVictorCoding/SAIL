package dev.sail.app.feature.dialogue

import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.jsonPrimitive
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class VoiceProtocolTest {
    @Test
    fun parsesTheTurnLifecycle() {
        assertEquals(VoiceEvent.Session("s1"), VoiceProtocol.parse("""{"type":"session","session_id":"s1","sample_rate":16000}"""))
        assertEquals(VoiceEvent.Transcript("hel", false), VoiceProtocol.parse("""{"type":"transcript","text":"hel","is_final":false}"""))
        assertEquals(VoiceEvent.TurnStarted("hello"), VoiceProtocol.parse("""{"type":"turn_started","transcript":"hello","epoch":3}"""))
        assertEquals(VoiceEvent.ResponseDelta("Hi"), VoiceProtocol.parse("""{"type":"response_delta","text":"Hi","token_count":1}"""))
        assertEquals(VoiceEvent.AudioChunk("UklG"), VoiceProtocol.parse("""{"type":"audio_chunk","audio_base64":"UklG","sample_rate":24000}"""))
        assertEquals(VoiceEvent.TurnFinished, VoiceProtocol.parse("""{"type":"turn_finished","latency_ms":900}"""))
    }

    @Test
    fun toolNamesUseTheColonForm() {
        assertEquals(VoiceEvent.ToolActivity("fs:read", true), VoiceProtocol.parse("""{"type":"tool_started","tool":"fs_read"}"""))
        assertEquals(
            VoiceEvent.ToolActivity("os:status", false),
            VoiceProtocol.parse("""{"type":"tool_event","event":{"tool":"os_status","state":"completed"}}"""),
        )
        assertEquals(
            VoiceEvent.ToolResult("web:read", false, "timeout"),
            VoiceProtocol.parse("""{"type":"tool_result","result":{"tool":"web:read","success":false,"output":{"error":"timeout"}}}"""),
        )
    }

    @Test
    fun approvalsFromBothMessageKinds() {
        assertEquals(
            VoiceEvent.ApprovalRequired("a1", null, "high", "Delete files"),
            VoiceProtocol.parse("""{"type":"approval_required","session_id":"a1","risk_level":"high","rationale":"Delete files"}"""),
        )
        val fromTool = VoiceProtocol.parse("""{"type":"tool_event","event":{"tool":"fs_write","state":"pending","approval_session":"a2"}}""")
        assertTrue(fromTool is VoiceEvent.ApprovalRequired && fromTool.sessionId == "a2" && fromTool.tool == "fs:write")
    }

    @Test
    fun snapshotRequestsAndCanvas() {
        assertEquals(
            VoiceEvent.SnapshotRequest("r1", "camera", "What's on the desk?"),
            VoiceProtocol.parse("""{"type":"snapshot_request","request_id":"r1","source":"camera","question":"What's on the desk?"}"""),
        )
        assertEquals(
            VoiceEvent.Canvas("json", "{}"),
            VoiceProtocol.parse("""{"type":"canvas","artifact":{"artifact_type":"json","content":"{}"}}"""),
        )
    }

    @Test
    fun badInputIsNeverACrash() {
        assertTrue(VoiceProtocol.parse("not json") is VoiceEvent.Error)
        assertEquals(VoiceEvent.Ignored, VoiceProtocol.parse("""{"type":"telemetry","rms":0.1}"""))
        assertEquals(VoiceEvent.Ignored, VoiceProtocol.parse("""{"type":"audio_chunk"}"""))
    }

    @Test
    fun clientActionsMatchVoiceJs() {
        val text = Json.parseToJsonElement(VoiceProtocol.text("hi", "QUJD")) as JsonObject
        assertEquals("text", text["action"]!!.jsonPrimitive.content)
        assertEquals("image/jpeg", (text["vision"] as JsonObject)["format"]!!.jsonPrimitive.content)
        assertEquals("""{"action":"end_turn"}""", VoiceProtocol.action("end_turn"))
        val declined = Json.parseToJsonElement(VoiceProtocol.snapshotDeclined("r1")) as JsonObject
        assertEquals("r1", declined["request_id"]!!.jsonPrimitive.content)
    }
}
