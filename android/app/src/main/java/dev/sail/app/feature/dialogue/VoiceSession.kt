package dev.sail.app.feature.dialogue

import dev.sail.app.core.net.SailApi
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.receiveAsFlow
import kotlinx.coroutines.launch
import okhttp3.Response
import okhttp3.WebSocket
import okhttp3.WebSocketListener
import okio.ByteString
import okio.ByteString.Companion.toByteString

enum class SocketState { Connecting, Open, Closed }

/**
 * One /ws/voice connection that reconnects by itself (2 s, then backing off to 15 s) until
 * [close]. Server messages arrive on [events] in order.
 */
class VoiceSession(private val api: SailApi, private val scope: CoroutineScope) {
    private val incoming = Channel<VoiceEvent>(Channel.UNLIMITED)
    val events: Flow<VoiceEvent> = incoming.receiveAsFlow()

    private val mutableState = MutableStateFlow(SocketState.Closed)
    val state: StateFlow<SocketState> = mutableState.asStateFlow()

    @Volatile private var socket: WebSocket? = null
    private var generation = 0
    private var closed = false
    private var retryJob: Job? = null
    private var failures = 0

    fun open() {
        closed = false
        connect()
    }

    fun close() {
        closed = true
        retryJob?.cancel()
        socket?.close(1000, "closed")
        socket = null
        mutableState.value = SocketState.Closed
    }

    /** Sends a JSON action; false when the socket isn't open (the caller tells the owner). */
    fun send(json: String): Boolean = mutableState.value == SocketState.Open && socket?.send(json) == true

    /**
     * Sends one microphone frame. Drops it and returns false if the connection is backed up,
     * like voice.js does above 64 KB buffered.
     */
    fun sendAudio(frame: ByteArray): Boolean {
        val ws = socket ?: return false
        if (mutableState.value != SocketState.Open || ws.queueSize() > 64_000) return false
        return ws.send(frame.toByteString())
    }

    private fun connect() {
        if (closed) return
        val mine = ++generation
        mutableState.value = SocketState.Connecting
        val listener = object : WebSocketListener() {
            override fun onOpen(webSocket: WebSocket, response: Response) {
                if (mine != generation) return
                failures = 0
                mutableState.value = SocketState.Open
            }

            override fun onMessage(webSocket: WebSocket, text: String) {
                if (mine == generation) incoming.trySend(VoiceProtocol.parse(text))
            }

            override fun onMessage(webSocket: WebSocket, bytes: ByteString) = Unit

            override fun onClosing(webSocket: WebSocket, code: Int, reason: String) {
                webSocket.close(1000, null)
            }

            override fun onClosed(webSocket: WebSocket, code: Int, reason: String) {
                lost(mine)
            }

            override fun onFailure(webSocket: WebSocket, t: Throwable, response: Response?) {
                lost(mine)
            }
        }
        socket = api.webSocket("/ws/voice", listener)
        if (socket == null) {
            mutableState.value = SocketState.Closed
            incoming.trySend(VoiceEvent.Error("SAIL isn't set up yet"))
        }
    }

    private fun lost(mine: Int) {
        if (mine != generation) return
        socket = null
        mutableState.value = SocketState.Closed
        if (closed) return
        failures++
        val wait = minOf(15_000L, 2_000L * failures)
        retryJob?.cancel()
        retryJob = scope.launch {
            delay(wait)
            connect()
        }
    }
}
