package dev.sail.app.feature.calendar

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import dev.sail.app.core.net.ApiResult
import dev.sail.app.core.net.SailApi
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import java.time.LocalDate
import java.time.ZoneOffset

data class CalendarUiState(
    val loading: Boolean = true,
    val selectedDate: LocalDate = LocalDate.now(),
    val dayView: Boolean = false,
    val showSources: Boolean = false,
    val sources: List<Source> = emptyList(),
    val window: EventsWindow = EventsWindow(),
    val preferences: Preferences = Preferences(),
    val selectedEvent: Event? = null,
    val showCreateSheet: Boolean = false,
    val error: String? = null,
    val banner: String? = null,
    val busy: Boolean = false,
)

class CalendarViewModel(
    private val api: SailApi,
) : ViewModel() {
    private val _state = MutableStateFlow(CalendarUiState())
    val state: StateFlow<CalendarUiState> = _state.asStateFlow()

    init {
        refresh()
    }

    fun selectDate(date: LocalDate) {
        _state.update { it.copy(selectedDate = date) }
        refreshWindow()
    }

    fun toggleDayView(day: Boolean) {
        _state.update { it.copy(dayView = day) }
    }

    fun toggleSources() {
        _state.update { it.copy(showSources = !it.showSources) }
    }

    fun openCreate() {
        _state.update { it.copy(showCreateSheet = true, selectedEvent = null) }
    }

    fun closeSheets() {
        _state.update { it.copy(showCreateSheet = false, selectedEvent = null) }
    }

    fun openEvent(eventId: String) {
        viewModelScope.launch {
            _state.update { it.copy(busy = true, error = null) }
            when (val res = api.calendarEvent(eventId)) {
                is ApiResult.Ok -> _state.update { it.copy(busy = false, selectedEvent = res.data) }
                is ApiResult.Err -> _state.update { it.copy(busy = false, error = res.message) }
            }
        }
    }

    fun refresh() {
        viewModelScope.launch {
            _state.update { it.copy(loading = true, error = null) }
            val srcs = when (val res = api.calendarSources()) {
                is ApiResult.Ok -> res.data.sources
                is ApiResult.Err -> {
                    _state.update { it.copy(loading = false, error = res.message) }
                    return@launch
                }
            }
            val prefs = when (val res = api.calendarPreferences()) {
                is ApiResult.Ok -> res.data
                is ApiResult.Err -> _state.value.preferences
            }
            _state.update { it.copy(sources = srcs, preferences = prefs) }
            refreshWindow()
        }
    }

    fun refreshWindow() {
        val anchor = _state.value.selectedDate
        val start = anchor.withDayOfMonth(1).minusDays(7).atStartOfDay(ZoneOffset.UTC).toInstant().toString()
        val end = anchor.plusMonths(1).withDayOfMonth(1).plusDays(7).atStartOfDay(ZoneOffset.UTC).toInstant().toString()
        viewModelScope.launch {
            when (val res = api.calendarEvents(start, end)) {
                is ApiResult.Ok -> _state.update { it.copy(loading = false, window = res.data, error = null) }
                is ApiResult.Err -> _state.update { it.copy(loading = false, error = res.message) }
            }
        }
    }

    fun createEvent(draft: EventDraft) {
        viewModelScope.launch {
            _state.update { it.copy(busy = true, error = null) }
            when (val res = api.createCalendarEvent(draft)) {
                is ApiResult.Ok -> {
                    _state.update { it.copy(busy = false, showCreateSheet = false, banner = "Event saved.") }
                    refresh()
                }
                is ApiResult.Err -> _state.update { it.copy(busy = false, error = res.message) }
            }
        }
    }

    fun togglePrepare(event: Event) {
        viewModelScope.launch {
            _state.update { it.copy(busy = true, error = null) }
            when (val res = api.patchCalendarEvent(event.id, EventPatch(prepare = !event.prepare))) {
                is ApiResult.Ok -> {
                    _state.update { it.copy(busy = false, selectedEvent = res.data) }
                    refreshWindow()
                }
                is ApiResult.Err -> _state.update { it.copy(busy = false, error = res.message) }
            }
        }
    }

    fun deleteEvent(eventId: String) {
        viewModelScope.launch {
            _state.update { it.copy(busy = true, error = null) }
            when (val res = api.deleteCalendarEvent(eventId)) {
                is ApiResult.Ok -> {
                    _state.update { it.copy(busy = false, selectedEvent = null, banner = "Event deleted.") }
                    refresh()
                }
                is ApiResult.Err -> _state.update { it.copy(busy = false, error = res.message) }
            }
        }
    }

    fun addFeed(name: String, url: String) {
        viewModelScope.launch {
            _state.update { it.copy(busy = true, error = null) }
            when (val res = api.addCalendarFeed(NewFeed(name.trim(), url.trim()))) {
                is ApiResult.Ok -> {
                    _state.update { it.copy(busy = false, banner = "Feed added.") }
                    refresh()
                }
                is ApiResult.Err -> _state.update { it.copy(busy = false, error = res.message) }
            }
        }
    }

    fun syncSource(id: String) {
        viewModelScope.launch {
            _state.update { it.copy(busy = true, error = null) }
            when (val res = api.syncCalendarSource(id)) {
                is ApiResult.Ok -> {
                    _state.update { it.copy(busy = false, banner = "Synced ${res.data.name}.") }
                    refresh()
                }
                is ApiResult.Err -> _state.update { it.copy(busy = false, error = res.message) }
            }
        }
    }

    fun removeSource(id: String) {
        viewModelScope.launch {
            _state.update { it.copy(busy = true, error = null) }
            when (val res = api.deleteCalendarSource(id)) {
                is ApiResult.Ok -> {
                    _state.update { it.copy(busy = false, banner = "Source removed.") }
                    refresh()
                }
                is ApiResult.Err -> _state.update { it.copy(busy = false, error = res.message) }
            }
        }
    }

    fun onWsChanged(msg: WsCalendarChanged) {
        if (msg.revision > _state.value.window.revision) {
            refresh()
        }
    }
}
