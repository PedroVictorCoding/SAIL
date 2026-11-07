package dev.sail.app.core.net

import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.serialization.json.JsonElement

sealed interface ConnectionState {
    data object NotConfigured : ConnectionState
    data object Checking : ConnectionState
    data object Online : ConnectionState
    data class Offline(val message: String) : ConnectionState
    data object Unauthorized : ConnectionState
}

/**
 * Polls "/api/health" while the app is in the foreground, and checks right away when asked
 * (after settings change, on resume). [start] and [stop] follow the activity lifecycle.
 */
class Connection(
    private val api: SailApi,
    private val settings: ServerSettings,
    private val scope: CoroutineScope,
    private val intervalMs: Long = 30_000,
) {
    private val mutable = MutableStateFlow<ConnectionState>(ConnectionState.Checking)
    val state: StateFlow<ConnectionState> = mutable.asStateFlow()
    private var job: Job? = null

    fun start() {
        if (job?.isActive == true) return
        job = scope.launch {
            while (isActive) {
                check()
                delay(intervalMs)
            }
        }
    }

    fun stop() {
        job?.cancel()
        job = null
    }

    fun refresh() {
        scope.launch { check() }
    }

    private suspend fun check() {
        if (settings.config.value == null) {
            mutable.value = ConnectionState.NotConfigured
            return
        }
        if (mutable.value !is ConnectionState.Online) mutable.value = ConnectionState.Checking
        mutable.value = when (val result = api.get("/api/health", JsonElement.serializer())) {
            is ApiResult.Ok -> ConnectionState.Online
            is ApiResult.Err -> when {
                result.unauthorized -> ConnectionState.Unauthorized
                else -> ConnectionState.Offline(result.message)
            }
        }
    }
}
