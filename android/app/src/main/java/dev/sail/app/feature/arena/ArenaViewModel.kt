package dev.sail.app.feature.arena

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import dev.sail.app.core.UiState
import dev.sail.app.core.toUiState
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch

class ArenaViewModel(private val repository: ArenaService) : ViewModel() {
    private val mutableCatalog = MutableStateFlow<UiState<Catalog>>(UiState.Loading)
    val catalog = mutableCatalog.asStateFlow()
    private val mutableDetail = MutableStateFlow<UiState<CourseDetail>>(UiState.Loading)
    val detail = mutableDetail.asStateFlow()
    private val mutableNext = MutableStateFlow<UiState<NextAction>>(UiState.Loading)
    val next = mutableNext.asStateFlow()
    private val mutableEvidence = MutableStateFlow<UiState<EvidenceList>>(UiState.Loading)
    val evidence = mutableEvidence.asStateFlow()
    private val mutableQueue = MutableStateFlow<UiState<ReviewQueue>>(UiState.Loading)
    val queue = mutableQueue.asStateFlow()

    fun loadCatalog() {
        viewModelScope.launch {
            mutableCatalog.value = UiState.Loading
            mutableCatalog.value = repository.catalog().toUiState("No courses available", isEmpty = { it.courses.isEmpty() })
        }
    }

    fun loadCourse(id: String) {
        viewModelScope.launch {
            mutableDetail.value = UiState.Loading
            mutableDetail.value = repository.course(id).toUiState()
        }
        viewModelScope.launch { mutableNext.value = repository.next(id).toUiState() }
        loadEvidence(id)
    }

    fun loadEvidence(id: String) {
        viewModelScope.launch {
            mutableEvidence.value = UiState.Loading
            mutableEvidence.value = repository.evidence(id).toUiState("No evidence yet", isEmpty = { it.objectives.isEmpty() })
        }
    }

    fun loadReviews(id: String) {
        viewModelScope.launch {
            mutableQueue.value = UiState.Loading
            // An empty queue still carries the authoritative catch-up plan.
            mutableQueue.value = repository.reviews(id).toUiState()
        }
    }
}
