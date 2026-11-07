package dev.sail.app.feature.arena

import dev.sail.app.core.net.ApiResult
import dev.sail.app.core.net.SailApi
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put
import java.net.URLEncoder
import java.nio.charset.StandardCharsets

interface ArenaService {
    suspend fun catalog(): ApiResult<Catalog>
    suspend fun course(id: String): ApiResult<CourseDetail>
    suspend fun next(id: String): ApiResult<NextAction>
    suspend fun evidence(id: String): ApiResult<EvidenceList>
    suspend fun reviews(id: String): ApiResult<ReviewQueue>
    suspend fun lesson(id: String): ApiResult<LessonView>
    suspend fun diagnostic(course: String, session: String?): ApiResult<DiagnosticView>
    suspend fun report(id: String): ApiResult<EvidenceList>
    suspend fun attempt(mode: PlayerMode, id: String, request: AttemptRequest): ApiResult<Grade>
    suspend fun hint(mode: PlayerMode, id: String, step: String): ApiResult<Hint>
    suspend fun reveal(id: String, step: String): ApiResult<Reveal>
    suspend fun advance(id: String): ApiResult<Gate>
}

class ArenaRepository(private val api: SailApi) : ArenaService {
    private fun encoded(id: String) = URLEncoder.encode(id, StandardCharsets.UTF_8.name())
    private fun resource(mode: PlayerMode, id: String) = "/api/learn/${mode.path}/${encoded(id)}"

    override suspend fun catalog() = api.get<Catalog>("/api/learn/courses")
    override suspend fun course(id: String) = api.get<CourseDetail>("/api/learn/courses/${encoded(id)}")
    override suspend fun next(id: String) = api.get<NextAction>("/api/learn/next?course=${encoded(id)}")
    override suspend fun evidence(id: String) = api.get<EvidenceList>("/api/learn/skills/evidence?course=${encoded(id)}")
    override suspend fun reviews(id: String) = api.get<ReviewQueue>("/api/learn/reviews/due?course=${encoded(id)}")
    override suspend fun lesson(id: String) = api.get<LessonView>("/api/learn/lessons/${encoded(id)}")
    override suspend fun diagnostic(course: String, session: String?): ApiResult<DiagnosticView> =
        if (session != null) api.get("/api/learn/diagnostics/${encoded(session)}")
        else api.postJson("/api/learn/diagnostics", buildJsonObject { put("course_id", course) })
    override suspend fun report(id: String) = api.get<EvidenceList>("/api/learn/diagnostics/${encoded(id)}/report")
    override suspend fun attempt(mode: PlayerMode, id: String, request: AttemptRequest) =
        api.postJson<AttemptRequest, Grade>("${resource(mode, id)}/attempt", request)
    override suspend fun hint(mode: PlayerMode, id: String, step: String): ApiResult<Hint> =
        if (mode == PlayerMode.Review) api.post("${resource(mode, id)}/hint")
        else api.postJson("${resource(mode, id)}/hint", buildJsonObject { put("step_id", step) })
    override suspend fun reveal(id: String, step: String) =
        api.postJson<kotlinx.serialization.json.JsonObject, Reveal>(
            "/api/learn/lessons/${encoded(id)}/reveal", buildJsonObject { put("step_id", step) },
        )
    override suspend fun advance(id: String) = api.post<Gate>("/api/learn/lessons/${encoded(id)}/advance")
}
