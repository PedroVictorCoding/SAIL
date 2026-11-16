package dev.sail.app.feature.dock

import dev.sail.app.feature.dialogue.VoiceEvent
import dev.sail.app.feature.dialogue.VoiceProtocol
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.boolean
import kotlinx.serialization.json.encodeToJsonElement
import kotlinx.serialization.json.int
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class DockPresenceTest {
    private val json = Json { ignoreUnknownKeys = true }

    private fun fixture(name: String): String =
        requireNotNull(javaClass.getResourceAsStream("/fixtures/presence/$name")) {
            "Missing fixture: $name"
        }.bufferedReader().readText()

    @Test
    fun parsesAllPresenceAndAnnounceFixtures() {
        val speak = VoiceProtocol.parse(fixture("announce_speak.json"))
        assertTrue(speak is VoiceEvent.Announce)
        speak as VoiceEvent.Announce
        assertEquals("a1b2", speak.itemId)
        assertEquals("Forge: refactor failed", speak.title)
        assertTrue(speak.speak)
        assertEquals("/#forge", speak.link)
        assertEquals(listOf("urgency 0.70", "owner project"), speak.reasons)

        val show = VoiceProtocol.parse(fixture("announce_show.json"))
        assertTrue(show is VoiceEvent.Announce)
        show as VoiceEvent.Announce
        assertEquals("c3d4", show.itemId)
        assertEquals(false, show.speak)
        assertNull(show.link)

        val greeting = VoiceProtocol.parse(fixture("presence_greeting.json"))
        assertTrue(greeting is VoiceEvent.PresenceGreeting)
        greeting as VoiceEvent.PresenceGreeting
        assertEquals(
            listOf(
                GreetingItem("a1b2", "Forge: refactor failed"),
                GreetingItem("c3d4", "Algorithms quiz tomorrow"),
            ),
            greeting.items,
        )
        assertEquals(5400L, greeting.awayForSecs)

        assertEquals(VoiceEvent.HandsFreeState("listening"), VoiceProtocol.parse(fixture("hands_free_listening.json")))
        assertEquals(VoiceEvent.HandsFreeState("follow_up"), VoiceProtocol.parse(fixture("hands_free_follow_up.json")))
        assertEquals(VoiceEvent.HandsFreeState("off"), VoiceProtocol.parse(fixture("hands_free_off.json")))
        assertEquals(VoiceEvent.NotAddressed, VoiceProtocol.parse(fixture("not_addressed.json")))

        val stateObj = json.decodeFromString<PresenceState>(fixture("presence_state.json"))
        assertEquals(Place.DOCK, stateObj.place)
        assertEquals("dock-7c1e", stateObj.activeDevice)
        assertEquals(1, stateObj.surfaces.size)
        assertTrue(stateObj.surfaces.first().canSpeak)

        val wrappedState = VoiceProtocol.parse("""{"type":"presence_state","state":${fixture("presence_state_asleep.json")}}""")
        assertTrue(wrappedState is VoiceEvent.PresenceStateEvent)
        assertEquals(Place.ASLEEP, (wrappedState as VoiceEvent.PresenceStateEvent).state.place)

        val settings = json.decodeFromString<PresenceSettings>(fixture("presence_settings.json"))
        assertEquals(600L, settings.awayAfterSecs)
        assertEquals(2700L, settings.greetAfterAwaySecs)
        assertEquals(3, settings.maxSpokenPerHour)
        assertEquals(listOf(SurfaceKind.WEB, SurfaceKind.DOCK), settings.speakOn)
    }

    @Test
    fun unknownTypeIgnoredAndMissingFieldsTolerated() {
        assertEquals(VoiceEvent.Ignored, VoiceProtocol.parse("""{"type":"future_presence_v2","x":1}"""))
        val minimalAnnounce = VoiceProtocol.parse("""{"type":"announce","item_id":"x1"}""")
        assertTrue(minimalAnnounce is VoiceEvent.Announce)
        minimalAnnounce as VoiceEvent.Announce
        assertEquals("x1", minimalAnnounce.itemId)
        assertEquals("", minimalAnnounce.title)
        assertEquals(emptyList<String>(), minimalAnnounce.reasons)

        assertEquals(VoiceEvent.HandsFreeState("off"), VoiceProtocol.parse("""{"type":"hands_free"}"""))
    }

    @Test
    fun dockViewModelReducerHandlesAsleepGreetingAnnounceAndNotAddressed() {
        var state = DockUiState(charging = true)
        assertTrue(state.charging)

        val greeting = VoiceProtocol.parse(fixture("presence_greeting.json"))
        state = DockViewModel.reduce(state, greeting)
        assertEquals("Welcome back. Forge: refactor failed, and Quiz 3 is due in 14 hours.", state.greeting?.spoken)
        assertEquals("speaking", state.orbState)

        val announce = VoiceProtocol.parse(fixture("announce_speak.json"))
        state = DockViewModel.reduce(state, announce)
        assertEquals(1, state.announcements.size)
        assertEquals("a1b2", state.announcements.first().itemId)

        val asleep = VoiceProtocol.parse("""{"type":"presence_state","state":${fixture("presence_state_asleep.json")}}""")
        state = DockViewModel.reduce(state, asleep)
        assertEquals(Place.ASLEEP, state.place)

        val beforeNotAddressed = state
        state = DockViewModel.reduce(state, VoiceEvent.NotAddressed)
        assertEquals(beforeNotAddressed, state)
    }

    @Test
    fun heartbeatPayloadAndSurfaceHelloMatchContract() {
        val req = Heartbeats.buildHeartbeatPayload(
            deviceId = "dock-7c1e",
            surface = SurfaceKind.DOCK,
            visible = true,
            inputIdleSecs = 99_999,
            charging = true,
            voiceReady = true,
        )
        val encoded = json.encodeToJsonElement(req).jsonObject
        assertEquals("dock-7c1e", encoded["device_id"]!!.jsonPrimitive.content)
        assertEquals("dock", encoded["surface"]!!.jsonPrimitive.content)
        assertEquals(true, encoded["visible"]!!.jsonPrimitive.boolean)
        assertEquals(86_400, encoded["input_idle_secs"]!!.jsonPrimitive.int)
        assertEquals(true, encoded["charging"]!!.jsonPrimitive.boolean)
        assertEquals(true, encoded["voice_ready"]!!.jsonPrimitive.boolean)

        val hello = json.parseToJsonElement(VoiceProtocol.surface("dock-7c1e", "dock", true)) as JsonObject
        assertEquals("surface", hello["type"]!!.jsonPrimitive.content)
        assertEquals("dock-7c1e", hello["device_id"]!!.jsonPrimitive.content)
        assertEquals("dock", hello["surface"]!!.jsonPrimitive.content)
        assertEquals(true, hello["hands_free"]!!.jsonPrimitive.boolean)
    }

    @Test
    fun callingStartTwiceStartsCaptureOnce() {
        var running = false
        var starts = 0
        val startCapture = {
            starts++
            running = true
            true
        }
        assertTrue(DockViewModel.startCaptureOnce(isRunning = { running }, startCapture = startCapture))
        assertTrue(DockViewModel.startCaptureOnce(isRunning = { running }, startCapture = startCapture))
        assertEquals(1, starts)
    }
}
