package dev.sail.app.feature.workspace

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import dev.sail.app.core.net.ApiResult
import kotlinx.coroutines.Job
import kotlinx.coroutines.async
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch

class WorkspaceViewModel(private val repository: WorkspaceSource) : ViewModel() {
    private val mutable = MutableStateFlow(WorkspaceData(loading = true))
    val state: StateFlow<WorkspaceData> = mutable.asStateFlow()
    private var pollJob: Job? = null

    fun start() {
        if (pollJob?.isActive == true) return
        pollJob = viewModelScope.launch {
            while (isActive) {
                load()
                delay(POLL_MS)
            }
        }
    }

    fun stop() {
        pollJob?.cancel()
        pollJob = null
    }

    fun refresh() {
        viewModelScope.launch { load() }
    }

    private suspend fun load() {
        mutable.update { it.copy(loading = true) }
        val snapshot = coroutineScope {
            val health = async { repository.health() }
            val mind = async { repository.mind() }
            val metrics = async { repository.metrics() }
            val catalog = async { repository.models() }
            val plans = async { repository.plans() }
            Snapshot(
                health = health.await(),
                mind = mind.await(),
                metrics = metrics.await(),
                catalog = catalog.await(),
                plans = plans.await(),
            )
        }
        mutable.value = WorkspaceData(
            health = snapshot.health,
            mind = snapshot.mind,
            metrics = snapshot.metrics,
            catalog = snapshot.catalog,
            plans = snapshot.plans,
            updatedAt = System.currentTimeMillis(),
            loading = false,
        )
    }

    private data class Snapshot(
        val health: ApiResult<Health>,
        val mind: ApiResult<MindStatus>,
        val metrics: ApiResult<SystemMetrics>,
        val catalog: ApiResult<ModelCatalog>,
        val plans: ApiResult<PlansResponse>,
    )

    companion object {
        private const val POLL_MS = 15_000L
    }
}
