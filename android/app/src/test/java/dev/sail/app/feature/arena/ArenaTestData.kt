package dev.sail.app.feature.arena

import dev.sail.app.core.net.ApiResult
import dev.sail.app.core.net.SailApi
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.decodeFromJsonElement

internal object ArenaFixtures {
    val json = SailApi.defaultJson
    val sample: JsonObject = json.parseToJsonElement(
        requireNotNull(javaClass.getResourceAsStream("/fixtures/arena/courses_api_sample.json")).bufferedReader().use { it.readText() },
    ) as JsonObject
    inline fun <reified T> read(key: String): T = json.decodeFromJsonElement(sample.getValue(key))
}

internal class FakeArenaService : ArenaService {
    var catalogResult: ApiResult<Catalog> = ApiResult.Ok(ArenaFixtures.read("GET /courses"))
    var lessonResult: ApiResult<LessonView> = ApiResult.Ok(ArenaFixtures.read("GET /lessons/:id"))
    var attemptResult: ApiResult<Grade> = ApiResult.Ok(ArenaFixtures.read("POST /lessons/:id/attempt (correct, hinted)"))
    var gateResult: ApiResult<Gate> = ApiResult.Ok(Gate("blocked_by_review", listOf("Review is due"), false))
    val attempts = mutableListOf<AttemptRequest>()
    var diagnosticCalls = 0
    var reportCalls = 0
    var hintCalls = 0
    var lessonCalls = 0
    var currentDiagnostic: DiagnosticView = ArenaFixtures.read("POST /diagnostics")
    var reportResult: ApiResult<EvidenceList> = ApiResult.Ok(ArenaFixtures.read("GET /skills/evidence"))
    override suspend fun catalog() = catalogResult
    override suspend fun course(id: String) = ApiResult.Ok(ArenaFixtures.read<CourseDetail>("GET /courses/rust-systems"))
    override suspend fun next(id: String) = ApiResult.Ok(ArenaFixtures.read<NextAction>("GET /next"))
    override suspend fun evidence(id: String) = reportResult
    override suspend fun reviews(id: String) = ApiResult.Ok(ArenaFixtures.read<ReviewQueue>("GET /reviews/due"))
    override suspend fun lesson(id: String): ApiResult<LessonView> { lessonCalls++; return lessonResult }
    override suspend fun diagnostic(course: String, session: String?): ApiResult<DiagnosticView> {
        diagnosticCalls++
        return ApiResult.Ok(currentDiagnostic)
    }
    override suspend fun report(id: String): ApiResult<EvidenceList> { reportCalls++; return reportResult }
    override suspend fun attempt(mode: PlayerMode, id: String, request: AttemptRequest): ApiResult<Grade> {
        attempts += request
        return attemptResult
    }
    override suspend fun hint(mode: PlayerMode, id: String, step: String): ApiResult<Hint> {
        hintCalls++
        return ApiResult.Ok(ArenaFixtures.read("POST /lessons/:id/hint"))
    }
    override suspend fun reveal(id: String, step: String) = ApiResult.Ok(ArenaFixtures.read<Reveal>("POST /lessons/:id/reveal"))
    override suspend fun advance(id: String) = gateResult
}
