package dev.sail.app.feature.nightshift

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import dev.sail.app.core.net.ApiResult
import dev.sail.app.core.net.SailApi
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch

data class NightShiftUiState(
    val loading: Boolean = true,
    val data: NightShiftResponse = NightShiftResponse(),
    val error: String? = null,
    val banner: String? = null,
    val diffs: Map<String, String> = emptyMap(),
    val busyIds: Set<String> = emptySet(),
    val submittingAsk: Boolean = false,
)

fun upsertErrandInResponse(response: NightShiftResponse, errand: Errand): NightShiftResponse {
    val existing = response.errands.indexOfFirst { it.id == errand.id }
    val nextErrands = if (existing >= 0) {
        response.errands.toMutableList().also { it[existing] = errand }
    } else {
        listOf(errand) + response.errands
    }
    return response.copy(errands = nextErrands)
}

class NightShiftViewModel(
    private val api: SailApi,
) : ViewModel() {
    private val _state = MutableStateFlow(NightShiftUiState())
    val state: StateFlow<NightShiftUiState> = _state.asStateFlow()

    init {
        refresh()
    }

    fun refresh() {
        viewModelScope.launch {
            _state.update { it.copy(loading = true, error = null) }
            when (val res = api.nightShiftState()) {
                is ApiResult.Ok -> _state.update {
                    it.copy(loading = false, data = res.data, error = null)
                }
                is ApiResult.Err -> _state.update {
                    it.copy(loading = false, error = res.message)
                }
            }
        }
    }

    fun submitAsk(ask: String) {
        val trimmed = ask.trim()
        if (trimmed.isEmpty()) return
        viewModelScope.launch {
            _state.update { it.copy(submittingAsk = true, banner = null, error = null) }
            when (val res = api.createNightShiftErrand(trimmed)) {
                is ApiResult.Ok -> _state.update {
                    it.copy(
                        submittingAsk = false,
                        data = upsertErrandInResponse(it.data, res.data),
                        banner = "Got it. SAIL is drafting a work order.",
                    )
                }
                is ApiResult.Err -> _state.update {
                    it.copy(submittingAsk = false, error = res.message)
                }
            }
        }
    }

    fun approve(id: String, editedOrder: WorkOrder?) {
        runAction(id, "Approved for tonight.") { api.approveNightShiftErrand(id, editedOrder) }
    }

    fun reshape(id: String, note: String) {
        val trimmed = note.trim()
        if (trimmed.isEmpty()) return
        runAction(id, "Sent back for reshaping.") { api.reshapeNightShiftErrand(id, trimmed) }
    }

    fun redo(id: String, note: String) {
        val trimmed = note.trim()
        if (trimmed.isEmpty()) return
        runAction(id, "Queued for redo tonight.") { api.redoNightShiftErrand(id, trimmed) }
    }

    fun ship(id: String) {
        runAction(id, "Shipping branch and opening draft PR…") { api.shipNightShiftErrand(id) }
    }

    fun drop(id: String) {
        runAction(id, "Errand dropped.") { api.dropNightShiftErrand(id) }
    }

    fun toggleDiff(id: String) {
        if (_state.value.diffs.containsKey(id)) {
            _state.update { it.copy(diffs = it.diffs - id) }
            return
        }
        viewModelScope.launch {
            _state.update { it.copy(busyIds = it.busyIds + id, error = null) }
            when (val res = api.nightShiftDiff(id)) {
                is ApiResult.Ok -> _state.update {
                    it.copy(busyIds = it.busyIds - id, diffs = it.diffs + (id to res.data))
                }
                is ApiResult.Err -> _state.update {
                    it.copy(busyIds = it.busyIds - id, error = res.message)
                }
            }
        }
    }

    fun onWsErrand(errand: Errand) {
        _state.update { it.copy(data = upsertErrandInResponse(it.data, errand)) }
    }

    fun onWsMorning(report: MorningReport) {
        _state.update { it.copy(data = it.data.copy(morning = report)) }
    }

    private fun runAction(id: String, okBanner: String, block: suspend () -> ApiResult<Errand>) {
        viewModelScope.launch {
            _state.update { it.copy(busyIds = it.busyIds + id, banner = null, error = null) }
            when (val res = block()) {
                is ApiResult.Ok -> _state.update {
                    it.copy(
                        busyIds = it.busyIds - id,
                        data = upsertErrandInResponse(it.data, res.data),
                        banner = okBanner,
                    )
                }
                is ApiResult.Err -> _state.update {
                    it.copy(busyIds = it.busyIds - id, error = res.message)
                }
            }
        }
    }
}
