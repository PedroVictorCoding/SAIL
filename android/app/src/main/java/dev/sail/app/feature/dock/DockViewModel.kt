package dev.sail.app.feature.dock

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import dev.sail.app.core.net.ApiResult
import dev.sail.app.core.net.SailApi
import dev.sail.app.core.net.getOrNull
import dev.sail.app.feature.dialogue.MicCapture
import dev.sail.app.feature.dialogue.SocketState
import dev.sail.app.feature.dialogue.SpeechPlayer
import dev.sail.app.feature.dialogue.VoiceEvent
import dev.sail.app.feature.dialogue.VoiceProtocol
import dev.sail.app.feature.dialogue.VoiceSession
import dev.sail.app.feature.workspace.PlansResponse
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch

class DockViewModel(
    private val api: SailApi,
    private val heartbeats: Heartbeats,
) : ViewModel() {
    private val session = VoiceSession(api, viewModelScope)
    private val player = SpeechPlayer(
        onDrained = {
            session.send(VoiceProtocol.action("playback_finished"))
            mutableState.update { it.copy(orbState = if (it.listening) "listening" else "idle") }
        },
        onError = { msg -> mutableState.update { it.copy(error = msg) } },
    )
    private val mic = MicCapture(
        onFrame = { frame -> session.sendAudio(frame) },
        onError = { msg -> mutableState.update { it.copy(error = msg, listening = false) } },
    )

    private val mutableState = MutableStateFlow(DockUiState(charging = heartbeats.charging.value))
    val state: StateFlow<DockUiState> = mutableState.asStateFlow()

    init {
        viewModelScope.launch {
            heartbeats.charging.collect { charging ->
                mutableState.update { it.copy(charging = charging) }
            }
        }
        viewModelScope.launch {
            heartbeats.lastState.collect { presence ->
                if (presence != null) {
                    mutableState.update { it.copy(place = presence.place) }
                }
            }
        }
        viewModelScope.launch {
            session.state.collect { sock ->
                heartbeats.setSurface(SurfaceKind.DOCK, voiceReady = sock == SocketState.Open)
            }
        }
        viewModelScope.launch {
            session.events.collect { event -> handleEvent(event) }
        }
    }

    fun onForegroundStart(hasMicPermission: Boolean) {
        val deviceId = heartbeats.deviceId(SurfaceKind.DOCK)
        heartbeats.setSurface(SurfaceKind.DOCK, voiceReady = false)
        session.setSurfaceHello(deviceId, "dock", handsFree = mutableState.value.listening)
        session.open()
        if (hasMicPermission && mutableState.value.listening) {
            val started = startCaptureOnce(isRunning = { mic.isRunning }, startCapture = { mic.start() })
            mutableState.update {
                it.copy(
                    listening = started,
                    orbState = if (started) "listening" else "idle",
                )
            }
        }
        refreshContext()
    }

    fun onForegroundStop() {
        mic.stop()
        player.stop()
        session.close()
        heartbeats.setSurface(SurfaceKind.PHONE, voiceReady = false)
    }

    fun stopListening() {
        mic.stop()
        val deviceId = heartbeats.deviceId(SurfaceKind.DOCK)
        session.setSurfaceHello(deviceId, "dock", handsFree = false)
        mutableState.update { it.copy(listening = false, handsFreeState = "off", orbState = "idle") }
    }

    fun startListening(hasMicPermission: Boolean) {
        if (!hasMicPermission) {
            mutableState.update { it.copy(error = "Microphone permission is required to listen.") }
            return
        }
        val started = startCaptureOnce(isRunning = { mic.isRunning }, startCapture = { mic.start() })
        val deviceId = heartbeats.deviceId(SurfaceKind.DOCK)
        session.setSurfaceHello(deviceId, "dock", handsFree = started)
        mutableState.update {
            it.copy(
                listening = started,
                orbState = if (started) "listening" else "idle",
                error = null,
            )
        }
    }

    fun togglePushToTalk(hasMicPermission: Boolean) {
        heartbeats.noteTouch()
        if (mutableState.value.listening) {
            session.send(VoiceProtocol.action("end_turn"))
        } else {
            startListening(hasMicPermission)
        }
    }

    fun dismissGreeting() {
        heartbeats.noteTouch()
        mutableState.update { it.copy(greeting = null) }
    }

    fun sendFeedback(itemId: String, feedback: String) {
        heartbeats.noteTouch()
        viewModelScope.launch {
            when (val res = api.sendProactiveFeedback(itemId, feedback)) {
                is ApiResult.Ok -> mutableState.update { cur ->
                    cur.copy(announcements = cur.announcements.filterNot { it.itemId == itemId })
                }
                is ApiResult.Err -> mutableState.update { it.copy(error = res.message) }
            }
        }
    }

    fun refreshContext() {
        viewModelScope.launch {
            api.presenceState().getOrNull()?.let { presence ->
                mutableState.update { it.copy(place = presence.place) }
            }
            api.get<PulseRemindersResponse>("/api/pulse/reminders").getOrNull()?.let { resp ->
                val next = resp.reminders.firstOrNull { it.status == "due" || it.status == "pending" }
                mutableState.update { it.copy(nextReminder = next) }
            }
            api.get<PlansResponse>("/api/plans/").getOrNull()?.let { plans ->
                val running = plans.plans.firstOrNull { planView ->
                    val st = (planView.taskState ?: planView.plan.state).lowercase()
                    st == "running" || st == "executing" || st == "in_progress" || st == "active"
                }
                mutableState.update { it.copy(runningTaskGoal = running?.plan?.goal?.takeIf { g -> g.isNotBlank() }) }
            }
        }
    }

    private fun handleEvent(event: VoiceEvent) {
        if (event is VoiceEvent.AudioChunk) {
            player.enqueue(event.wavBase64)
            mutableState.update { it.copy(orbState = "speaking") }
            return
        }
        if (event is VoiceEvent.HandsFreeState && event.state == "follow_up") {
            heartbeats.noteAddressedTurn()
        }
        if (event is VoiceEvent.TurnStarted) {
            heartbeats.noteAddressedTurn()
        }
        mutableState.update { cur -> reduce(cur, event) }
    }

    override fun onCleared() {
        mic.stop()
        player.release()
        session.close()
        super.onCleared()
    }

    companion object {
        /** Idempotent guard so granting permission + ON_START never starts two mic captures. */
        fun startCaptureOnce(isRunning: () -> Boolean, startCapture: () -> Boolean): Boolean =
            if (isRunning()) true else startCapture()

        /** Pure state reducer for Dock events so JVM tests can verify state transitions directly. */
        fun reduce(cur: DockUiState, event: VoiceEvent): DockUiState = when (event) {
            is VoiceEvent.Announce -> {
                val item = DockAnnouncement(
                    itemId = event.itemId,
                    title = event.title,
                    body = event.body,
                    source = event.source,
                    kind = event.kind,
                    link = event.link,
                    speak = event.speak,
                    reasons = event.reasons,
                )
                val next = listOf(item) + cur.announcements.filterNot { it.itemId == item.itemId }
                cur.copy(announcements = next, orbState = if (event.speak) "speaking" else cur.orbState)
            }
            is VoiceEvent.PresenceGreeting -> cur.copy(
                greeting = DockGreeting(event.spoken, event.items, event.awayForSecs),
                orbState = "speaking",
            )
            is VoiceEvent.PresenceStateEvent -> cur.copy(place = event.state.place)
            is VoiceEvent.HandsFreeState -> cur.copy(handsFreeState = event.state)
            VoiceEvent.NotAddressed -> cur // No text stored or shown.
            is VoiceEvent.TurnStarted -> cur.copy(orbState = "thinking")
            is VoiceEvent.ResponseDelta -> cur.copy(orbState = "thinking")
            VoiceEvent.ResponseFinished -> cur.copy(orbState = "speaking")
            VoiceEvent.TurnFinished -> cur.copy(orbState = if (cur.listening) "listening" else "idle")
            is VoiceEvent.Control -> cur.copy(muted = event.muted)
            is VoiceEvent.Error -> cur.copy(error = event.message)
            else -> cur
        }
    }
}
