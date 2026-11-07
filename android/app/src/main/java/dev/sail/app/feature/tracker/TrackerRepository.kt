package dev.sail.app.feature.tracker

import dev.sail.app.core.net.ApiResult
import dev.sail.app.core.net.SailApi

class TrackerRepository(
    private val api: SailApi,
) {
    suspend fun fetchBoard(): ApiResult<TrackerBoardEnvelope> =
        api.get("/api/tracker/board")

    suspend fun refresh(source: String? = null): ApiResult<TrackerBoardEnvelope> =
        api.postJson("/api/tracker/refresh", TrackerRefreshRequest(source = source))

    fun tileUrl(z: Int, x: Int, y: Int): String? =
        api.url("/api/tracker/tiles/$z/$x/$y.png")?.toString()
}
