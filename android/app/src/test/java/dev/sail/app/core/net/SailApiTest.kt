package dev.sail.app.core.net

import kotlinx.coroutines.test.runTest
import kotlinx.serialization.Serializable
import okhttp3.HttpUrl.Companion.toHttpUrl
import okhttp3.mockwebserver.MockResponse
import okhttp3.mockwebserver.MockWebServer
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test

class SailApiTest {
    @Serializable
    data class Course(val id: String, val title: String)

    @Serializable
    data class Attempt(val answer: String)

    private lateinit var server: MockWebServer
    private lateinit var api: SailApi
    private val token = "t".repeat(40)

    @Before
    fun setUp() {
        server = MockWebServer()
        server.start()
        val base = server.url("/").toString().trimEnd('/').toHttpUrl()
        api = SailApi(config = { ServerConfig(base, token) })
    }

    @After
    fun tearDown() {
        server.shutdown()
    }

    @Test
    fun getDecodesJsonAndIgnoresUnknownFields() = runTest {
        server.enqueue(MockResponse().setBody("""{"id":"rust","title":"Rust","extra":1}"""))
        val result = api.get<Course>("/api/learn/courses/rust")
        assertEquals(ApiResult.Ok(Course("rust", "Rust")), result)
        val request = server.takeRequest()
        assertEquals("/api/learn/courses/rust", request.path)
        assertEquals("Bearer $token", request.getHeader("Authorization"))
    }

    @Test
    fun postJsonSendsTheBody() = runTest {
        server.enqueue(MockResponse().setBody("""{"id":"a","title":"b"}"""))
        api.postJson<Attempt, Course>("/api/learn/lessons/x/attempt", Attempt("move"))
        val request = server.takeRequest()
        assertEquals("POST", request.method)
        assertEquals("""{"answer":"move"}""", request.body.readUtf8())
    }

    @Test
    fun httpErrorsBecomeValuesWithTheServerMessage() = runTest {
        server.enqueue(MockResponse().setResponseCode(503).setBody("""{"error":"Runner unavailable"}"""))
        val result = api.get<Course>("/api/learn/courses/rust")
        assertEquals(ApiResult.Err(503, "Runner unavailable"), result)
    }

    @Test
    fun unauthorizedIsFlagged() = runTest {
        server.enqueue(MockResponse().setResponseCode(401))
        val result = api.get<Course>("/api/tracker/board") as ApiResult.Err
        assertTrue(result.unauthorized)
    }

    @Test
    fun unreadableBodyIsAnErrorNotACrash() = runTest {
        server.enqueue(MockResponse().setBody("<html>proxy error</html>"))
        val result = api.get<Course>("/api/learn/courses/rust") as ApiResult.Err
        assertEquals(0, result.status)
    }

    @Test
    fun unreachableServerIsStatusZero() = runTest {
        server.shutdown()
        val result = api.get<Course>("/api/health") as ApiResult.Err
        assertTrue(result.unreachable)
    }

    @Test
    fun notConfiguredMeansNoRequest() = runTest {
        val unconfigured = SailApi(config = { null })
        val result = unconfigured.get<Course>("/api/health") as ApiResult.Err
        assertEquals(0, result.status)
        assertNull(unconfigured.url("/api/health"))
    }

    @Test
    fun tokenIsNeverSentToAnotherHost() = runTest {
        val other = MockWebServer()
        other.start()
        other.enqueue(MockResponse().setBody("x"))
        val call = api.client.newCall(okhttp3.Request.Builder().url(other.url("/art.jpg").newBuilder().host("127.0.0.2").build()).build())
        runCatching { call.execute().close() }
        // Either the request never reached 127.0.0.2, or it did without the header.
        if (other.requestCount > 0) assertNull(other.takeRequest().getHeader("Authorization"))
        other.shutdown()
    }
}
