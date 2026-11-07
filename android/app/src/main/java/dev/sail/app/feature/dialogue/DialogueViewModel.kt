package dev.sail.app.feature.dialogue

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import dev.sail.app.core.net.ApiResult
import dev.sail.app.core.net.SailApi
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable

/** What the assistant is doing, from the server's events (never guessed). */
enum class Phase(val label: String) {
    Disconnected("Reconnecting…"),
    Idle("Ready"),
    Listening("Listening"),
    Transcribing("Transcribing"),
    Thinking("Thinking"),
    Working("Working"),
    Speaking("Speaking"),
    Approval("Approval needed"),
    Holding("Recording held"),
}

sealed interface ChatItem {
    val key: Long

    data class User(override val key: Long, val text: String, val imageJpeg: ByteArray? = null) : ChatItem
    data class Sail(override val key: Long, val text: String, val done: Boolean) : ChatItem
    data class Visual(override val key: Long, val type: String, val content: String) : ChatItem
    data class ToolOutcome(override val key: Long, val tool: String, val success: Boolean, val error: String?) : ChatItem
    data class Approval(
        override val key: Long,
        val sessionId: String,
        val tool: String?,
        val risk: String?,
        val rationale: String?,
        val status: ApprovalStatus = ApprovalStatus.Pending,
    ) : ChatItem
    data class Snapshot(override val key: Long, val requestId: String, val source: String, val question: String?, val open: Boolean = true) : ChatItem
}

enum class ApprovalStatus { Pending, Sending, Approved, Denied, Resolved }

data class DialogueUiState(
    val phase: Phase = Phase.Disconnected,
    val items: List<ChatItem> = emptyList(),
    val liveTranscript: String? = null,
    val activeTools: List<String> = emptyList(),
    val micOn: Boolean = false,
    val turnInProgress: Boolean = false,
    val error: String? = null,
)

@Serializable
private data class ResolveRequest(
    @SerialName("session_id") val sessionId: String,
    val approved: Boolean,
)

class DialogueViewModel(private val api: SailApi) : ViewModel() {
    private val mutable = MutableStateFlow(DialogueUiState())
    val state: StateFlow<DialogueUiState> = mutable.asStateFlow()

    private val session = VoiceSession(api, viewModelScope)
    private val player = SpeechPlayer(
        onDrained = { if (!mutable.value.turnInProgress) playbackFinished() },
        onError = ::showError,
    )
    private val mic = MicCapture(onFrame = ::onMicFrame, onError = { message ->
        mutable.update { it.copy(micOn = false) }
        showError(message)
    })
    private var nextKey = 0L
    private var replyKey: Long? = null
    private var pendingImage: ByteArray? = null
    // Written on the main thread, read by the speech player's thread.
    @Volatile private var turnFinished = false

    init {
        viewModelScope.launch { session.events.collect(::onEvent) }
        viewModelScope.launch {
            session.state.collect { socket ->
                if (socket != SocketState.Open) {
                    stopMic(sendCancel = false)
                    player.stop()
                    mutable.update { it.copy(phase = Phase.Disconnected, turnInProgress = false, activeTools = emptyList()) }
                } else {
                    mutable.update { it.copy(phase = Phase.Idle) }
                }
            }
        }
        session.open()
    }

    // Owner actions.

    fun sendText(text: String, jpeg: ByteArray? = null) {
        val trimmed = text.trim()
        if (trimmed.isEmpty() && jpeg == null) return
        val prompt = trimmed.ifEmpty { "What's in this picture?" }
        val image = jpeg?.let { android.util.Base64.encodeToString(it, android.util.Base64.NO_WRAP) }
        player.stop()
        if (!session.send(VoiceProtocol.text(prompt, image))) return showError("Not connected. Wait for SAIL to reconnect.")
        if (jpeg != null) {
            // turn_started echoes the text; keep the picture next to it.
            pendingImage = jpeg
        }
    }

    /** Called after RECORD_AUDIO is granted. */
    fun startMic() {
        if (session.state.value != SocketState.Open) return showError("Not connected. Wait for SAIL to reconnect.")
        if (mic.start()) mutable.update { it.copy(micOn = true, phase = if (it.turnInProgress) it.phase else Phase.Listening) }
    }

    fun stopMic(sendCancel: Boolean = true) {
        if (!mic.isRunning) return
        mic.stop()
        if (sendCancel) session.send(VoiceProtocol.action("speech_cancel"))
        mutable.update { it.copy(micOn = false, liveTranscript = null, phase = if (it.phase == Phase.Listening) Phase.Idle else it.phase) }
    }

    /** "Send turn": submit what was said without waiting for the pause. */
    fun endTurn() {
        session.send(VoiceProtocol.action("end_turn"))
    }

    fun interrupt() {
        player.stop()
        session.send(VoiceProtocol.action("interrupt"))
    }

    fun resolveApproval(sessionId: String, approved: Boolean) {
        setApproval(sessionId, ApprovalStatus.Sending)
        viewModelScope.launch {
            val result = api.post(
                "/api/guardian/resolve",
                ResolveRequest(sessionId, approved),
                ResolveRequest.serializer(),
                SailApi.anyJson,
            )
            when (result) {
                is ApiResult.Ok -> setApproval(sessionId, if (approved) ApprovalStatus.Approved else ApprovalStatus.Denied)
                is ApiResult.Err -> {
                    setApproval(sessionId, ApprovalStatus.Pending)
                    showError(
                        when {
                            result.unauthorized -> "Owner token not accepted."
                            result.unavailable -> "Owner approval service is not configured."
                            else -> "Approval expired, revoked, or unavailable."
                        },
                    )
                }
            }
        }
    }

    fun answerSnapshot(requestId: String, jpeg: ByteArray?) {
        val sent = if (jpeg == null) {
            session.send(VoiceProtocol.snapshotDeclined(requestId))
        } else {
            val image = android.util.Base64.encodeToString(jpeg, android.util.Base64.NO_WRAP)
            session.send(VoiceProtocol.snapshot(requestId, image))
        }
        if (!sent) return showError("Not connected. The picture wasn't sent.")
        mutable.update { s ->
            s.copy(
                items = s.items.map { if (it is ChatItem.Snapshot && it.requestId == requestId) it.copy(open = false) else it } +
                    listOfNotNull(jpeg?.let { ChatItem.User(key(), "Picture for SAIL", it) }),
                phase = if (jpeg != null) Phase.Thinking else s.phase,
            )
        }
    }

    fun dismissError() = mutable.update { it.copy(error = null) }

    // Server events.

    private fun onEvent(event: VoiceEvent) {
        when (event) {
            is VoiceEvent.Session -> {
                replyKey = null
                mutable.update { it.copy(items = emptyList(), phase = Phase.Idle, turnInProgress = false) }
            }
            is VoiceEvent.Transcript -> mutable.update { it.copy(liveTranscript = event.text) }
            VoiceEvent.TurnCommitted -> mutable.update { it.copy(turnInProgress = true, phase = Phase.Transcribing) }
            is VoiceEvent.TurnStarted -> {
                player.stop()
                turnFinished = false
                val reply = key()
                replyKey = reply
                val image = pendingImage
                pendingImage = null
                mutable.update {
                    it.copy(
                        items = it.items + ChatItem.User(key(), event.transcript, image) + ChatItem.Sail(reply, "", done = false),
                        liveTranscript = null,
                        turnInProgress = true,
                        phase = Phase.Thinking,
                    )
                }
            }
            is VoiceEvent.ResponseDelta -> appendReply(event.text, Phase.Thinking)
            is VoiceEvent.AckStarted -> if (event.text.isNotEmpty()) appendReply(event.text + " ", Phase.Speaking)
            VoiceEvent.ResponseFinished -> mutable.update { it.copy(phase = Phase.Speaking) }
            is VoiceEvent.AudioChunk -> player.enqueue(event.wavBase64)
            VoiceEvent.TurnFinished -> {
                turnFinished = true
                val reply = replyKey
                mutable.update { s ->
                    s.copy(
                        items = s.items.map { if (it is ChatItem.Sail && it.key == reply) it.copy(done = true) else it },
                        turnInProgress = false,
                        activeTools = emptyList(),
                        phase = when {
                            player.isPlaying -> Phase.Speaking
                            s.micOn -> Phase.Listening
                            else -> Phase.Idle
                        },
                    )
                }
                if (!player.isPlaying) playbackFinished()
            }
            is VoiceEvent.Interruption -> {
                player.stop()
                mutable.update { it.copy(turnInProgress = false, activeTools = emptyList(), phase = if (it.micOn) Phase.Listening else Phase.Idle) }
            }
            is VoiceEvent.ToolActivity -> mutable.update { s ->
                val tools = if (event.active) (s.activeTools - event.tool) + event.tool else s.activeTools - event.tool
                s.copy(activeTools = tools, phase = if (event.active) Phase.Working else if (s.turnInProgress) Phase.Thinking else s.phase)
            }
            is VoiceEvent.ToolResult -> mutable.update { s ->
                s.copy(
                    activeTools = s.activeTools - event.tool,
                    items = if (event.success) s.items else s.items + ChatItem.ToolOutcome(key(), event.tool, false, event.error),
                )
            }
            is VoiceEvent.Canvas -> mutable.update { it.copy(items = it.items + ChatItem.Visual(key(), event.type, event.content)) }
            is VoiceEvent.ApprovalRequired -> mutable.update { s ->
                if (s.items.any { it is ChatItem.Approval && it.sessionId == event.sessionId }) {
                    s.copy(phase = Phase.Approval)
                } else {
                    s.copy(
                        items = s.items + ChatItem.Approval(key(), event.sessionId, event.tool, event.risk, event.rationale),
                        phase = Phase.Approval,
                    )
                }
            }
            is VoiceEvent.ApprovalResolved -> {
                mutable.update { s ->
                    s.copy(
                        items = s.items.map {
                            if (it is ChatItem.Approval && it.sessionId == event.sessionId && it.status == ApprovalStatus.Pending) {
                                it.copy(status = ApprovalStatus.Resolved)
                            } else {
                                it
                            }
                        },
                        phase = if (s.phase == Phase.Approval) Phase.Thinking else s.phase,
                    )
                }
            }
            is VoiceEvent.SnapshotRequest -> mutable.update { s ->
                s.copy(
                    items = s.items.map { if (it is ChatItem.Snapshot) it.copy(open = false) else it } +
                        ChatItem.Snapshot(key(), event.requestId, event.source, event.question),
                )
            }
            is VoiceEvent.CaptureLimit -> {
                mic.stop()
                mutable.update { it.copy(micOn = false, phase = Phase.Holding, liveTranscript = event.message) }
            }
            is VoiceEvent.Control -> Unit
            is VoiceEvent.Error -> showError(event.message)
            VoiceEvent.Ignored -> Unit
        }
    }

    private fun appendReply(text: String, phase: Phase) {
        val reply = replyKey ?: key().also { newKey ->
            replyKey = newKey
            mutable.update { it.copy(items = it.items + ChatItem.Sail(newKey, "", done = false)) }
        }
        mutable.update { s ->
            s.copy(
                items = s.items.map { if (it is ChatItem.Sail && it.key == reply) it.copy(text = it.text + text) else it },
                phase = if (s.phase == Phase.Working) s.phase else phase,
            )
        }
    }

    private fun setApproval(sessionId: String, status: ApprovalStatus) = mutable.update { s ->
        s.copy(items = s.items.map { if (it is ChatItem.Approval && it.sessionId == sessionId) it.copy(status = status) else it })
    }

    private fun playbackFinished() {
        if (!turnFinished) return
        turnFinished = false
        session.send(VoiceProtocol.action("playback_finished"))
        mutable.update { it.copy(phase = if (it.micOn) Phase.Listening else if (it.phase == Phase.Disconnected) it.phase else Phase.Idle) }
    }

    // Mic frames go out only while SAIL is listening, never while it thinks or speaks, so
    // its own voice isn't sent back (voice.js does the same).
    private fun onMicFrame(frame: ByteArray) {
        val s = mutable.value
        if (s.turnInProgress || player.isPlaying) return
        if (!session.sendAudio(frame)) {
            viewModelScope.launch {
                stopMic(sendCancel = false)
                showError("The connection is too slow for audio; the microphone stopped.")
            }
        }
    }

    private fun showError(message: String) = mutable.update { it.copy(error = message) }

    private fun key(): Long = nextKey++

    override fun onCleared() {
        mic.stop()
        player.release()
        session.close()
    }
}
