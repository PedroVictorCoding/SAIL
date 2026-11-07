package dev.sail.app.feature.tracker

import dev.sail.app.core.net.ApiResult
import dev.sail.app.core.net.SailApi

open class TrackerRepository(
    private val api: SailApi,
) {
    open suspend fun fetchBoard(): ApiResult<TrackerBoardEnvelope> =
        api.get("/api/tracker/board")

    open suspend fun refresh(source: String? = null): ApiResult<TrackerBoardEnvelope> =
        api.postJson("/api/tracker/refresh", TrackerRefreshRequest(source = source))

    open fun tileUrl(z: Int, x: Int, y: Int): String? =
        api.url("/api/tracker/tiles/$z/$x/$y.png")?.toString()
}
