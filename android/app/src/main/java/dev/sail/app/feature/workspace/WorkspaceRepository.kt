package dev.sail.app.feature.workspace

import dev.sail.app.core.net.ApiResult
import dev.sail.app.core.net.SailApi

interface WorkspaceSource {
    suspend fun health(): ApiResult<Health>

    suspend fun mind(): ApiResult<MindStatus>

    suspend fun metrics(): ApiResult<SystemMetrics>

    suspend fun models(): ApiResult<ModelCatalog>

    suspend fun plans(): ApiResult<PlansResponse>
}

class WorkspaceRepository(private val api: SailApi) : WorkspaceSource {
    override suspend fun health(): ApiResult<Health> = api.get<Health>("/api/health")

    override suspend fun mind(): ApiResult<MindStatus> = api.get<MindStatus>("/api/mind/status")

    override suspend fun metrics(): ApiResult<SystemMetrics> = api.get<SystemMetrics>("/api/system/metrics")

    override suspend fun models(): ApiResult<ModelCatalog> = api.get<ModelCatalog>("/api/mind/models")

    override suspend fun plans(): ApiResult<PlansResponse> = api.get<PlansResponse>("/api/plans/")
}
