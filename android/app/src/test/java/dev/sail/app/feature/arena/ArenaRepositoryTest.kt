package dev.sail.app.feature.arena

import dev.sail.app.core.net.ApiResult
import dev.sail.app.core.net.SailApi
import dev.sail.app.core.net.ServerConfig
import kotlinx.coroutines.test.runTest
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.jsonObject
import okhttp3.mockwebserver.MockResponse
import okhttp3.mockwebserver.MockWebServer
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class ArenaRepositoryTest {
    private fun response(key: String) = MockResponse().setHeader("Content-Type", "application/json")
        .setBody(ArenaFixtures.sample.getValue(key).toString())

    @Test fun reviewHintHasEmptyBodyAndOwnerAuthorization() = runTest {
        MockWebServer().use { server ->
            server.enqueue(response("POST /lessons/:id/hint"))
            val repository = ArenaRepository(SailApi({ ServerConfig(server.url("/"), "test-owner") }))
            assertTrue(repository.hint(PlayerMode.Review, "review-one", "ignored") is ApiResult.Ok)
            val request = server.takeRequest()
            assertEquals("/api/learn/reviews/review-one/hint", request.path)
            assertEquals("POST", request.method)
            assertEquals(0L, request.bodySize)
            assertEquals("Bearer test-owner", request.getHeader("Authorization"))
        }
    }

    @Test fun resumingDiagnosticUsesGetOnRuntimeId() = runTest {
        MockWebServer().use { server ->
            server.enqueue(response("GET /diagnostics/:id"))
            val repository = ArenaRepository(SailApi({ ServerConfig(server.url("/"), "test-owner") }))
            repository.diagnostic("rust-systems", "runtime-id")
            val request = server.takeRequest()
            assertEquals("GET", request.method)
            assertEquals("/api/learn/diagnostics/runtime-id", request.path)
        }
    }

    @Test fun attemptsNeverSendGradingOrEvidenceDecisions() = runTest {
        MockWebServer().use { server ->
            server.enqueue(response("POST /lessons/:id/attempt (correct, hinted)"))
            val repository = ArenaRepository(SailApi({ ServerConfig(server.url("/"), "test-owner") }))
            val answer = kotlinx.serialization.json.buildJsonObject { put("text", JsonPrimitive("clone")) }
            repository.attempt(PlayerMode.Lesson, "lesson-one", AttemptRequest("stable-id", "step-one", answer, "low"))
            val request = server.takeRequest()
            val body = ArenaFixtures.json.parseToJsonElement(request.body.readUtf8()).jsonObject
            assertEquals(setOf("attempt_id", "step_id", "answer", "confidence"), body.keys)
            assertFalse(body.containsKey("result"))
            assertEquals(JsonPrimitive("stable-id"), body["attempt_id"])
        }
    }

    @Test fun refusedAuthenticationIsAnErrorValue() = runTest {
        MockWebServer().use { server ->
            server.enqueue(MockResponse().setResponseCode(401).setBody("{\"error\":\"Owner authentication required\"}"))
            val repository = ArenaRepository(SailApi({ ServerConfig(server.url("/"), "bad-token") }))
            val result = repository.catalog()
            assertEquals(ApiResult.Err(401, "Owner authentication required"), result)
        }
    }
}
