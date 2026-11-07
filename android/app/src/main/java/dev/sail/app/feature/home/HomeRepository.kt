package dev.sail.app.feature.home

import dev.sail.app.core.net.ApiResult
import dev.sail.app.core.net.SailApi
import dev.sail.app.core.net.map
import java.net.URLEncoder

class HomeRepository(
    private val api: SailApi,
) {
    suspend fun fetchBriefing(): ApiResult<ReturnBriefing?> =
        api.get<BriefingEnvelope>("/api/briefing/latest").map { it.briefing }

    suspend fun refreshBriefing(): ApiResult<ReturnBriefing?> =
        api.post<BriefingEnvelope>("/api/briefing/refresh").map { it.briefing }

    suspend fun dismissBriefing(): ApiResult<Boolean> =
        api.post<DismissBriefingResponse>("/api/briefing/actions/dismiss").map { it.dismissed }

    suspend fun playStagedMusic(uri: String): ApiResult<PlayMusicResponse> =
        api.postJson("/api/briefing/actions/play-music", PlayMusicRequest(uri = uri))

    suspend fun fetchSystemMetrics(): ApiResult<SystemMetrics> =
        api.get("/api/system/metrics")

    suspend fun fetchHealth(): ApiResult<HealthResponse> =
        api.get("/api/health")

    suspend fun fetchMindStatus(): ApiResult<HomeMindStatus> =
        api.get("/api/mind/status")

    suspend fun fetchCourses(): ApiResult<List<HomeCourseSummary>> =
        api.get<HomeCoursesResponse>("/api/learn/courses").map { it.courses }

    suspend fun fetchLearnNext(courseId: String): ApiResult<HomeLearnNext> {
        val encoded = URLEncoder.encode(courseId, Charsets.UTF_8.name())
        return api.get("/api/learn/next?course=$encoded")
    }

    suspend fun fetchSpotifyStatus(): ApiResult<SpotifyStatusResponse> =
        api.get("/api/spotify/status")
}
