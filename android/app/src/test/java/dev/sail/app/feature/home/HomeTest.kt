package dev.sail.app.feature.home

import dev.sail.app.core.UiState
import dev.sail.app.core.net.ApiResult
import dev.sail.app.core.net.SailApi
import dev.sail.app.core.net.ServerConfig
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.UnconfinedTestDispatcher
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.test.setMain
import okhttp3.mockwebserver.Dispatcher
import okhttp3.mockwebserver.MockResponse
import okhttp3.mockwebserver.MockWebServer
import okhttp3.mockwebserver.RecordedRequest
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test

@OptIn(ExperimentalCoroutinesApi::class)
class HomeTest {

    private lateinit var server: MockWebServer
    private val testDispatcher = UnconfinedTestDispatcher()

    @Before
    fun setUp() {
        Dispatchers.setMain(testDispatcher)
        server = MockWebServer()
        server.start()
    }

    @After
    fun tearDown() {
        server.shutdown()
        Dispatchers.resetMain()
    }

    private fun readFixture(path: String): String {
        val stream = javaClass.classLoader?.getResourceAsStream(path)
        requireNotNull(stream) { "Missing fixture: $path" }
        return stream.bufferedReader(Charsets.UTF_8).use { it.readText() }
    }

    private fun createApi(token: String = "test-owner-token"): SailApi {
        val baseUrl = server.url("/")
        return SailApi(config = { ServerConfig(baseUrl = baseUrl, token = token) })
    }

    @Test
    fun parsesHomeFixturesAndFormatsMetrics() {
        val json = SailApi.defaultJson

        val briefingEnvelope = json.decodeFromString(
            BriefingEnvelope.serializer(),
            readFixture("fixtures/home/briefing_latest.json"),
        )
        val briefing = briefingEnvelope.briefing
        assertNotNull(briefing)
        assertEquals("Welcome back", briefing?.greeting)
        assertEquals("Music for 18 Musicians", briefing?.stagedMusic?.name)
        assertEquals("SAIL", briefing?.projectContinuity?.activeWorkspace)
        assertEquals(1, briefing?.eventsToWatch?.size)
        assertEquals(1, briefing?.newsRadar?.size)

        val metrics = json.decodeFromString(
            SystemMetrics.serializer(),
            readFixture("fixtures/home/system_metrics.json"),
        )
        assertEquals("76.9 GB free", HomeFormatting.formatFreeStorage(metrics))
        assertEquals("2.0 / 30.0 GB", HomeFormatting.formatMemoryUsage(metrics))
        assertEquals("15%", HomeFormatting.formatCpuPercent(metrics.cpuPercent))

        val courses = json.decodeFromString(
            HomeCoursesResponse.serializer(),
            readFixture("fixtures/home/learn_courses.json"),
        ).courses
        assertEquals(1, courses.size)
        assertEquals("rust-systems", courses.first().id)
        assertEquals(3, courses.first().progress?.due)

        val spotify = json.decodeFromString(
            SpotifyStatusResponse.serializer(),
            readFixture("fixtures/home/spotify_status.json"),
        )
        assertTrue(spotify.connected)
        assertEquals("Avril 14th", spotify.nowPlaying?.name)
    }

    @Test
    fun repositorySendsAuthHeaderAndEncodesCourseNextQuery() = runTest {
        server.enqueue(
            MockResponse()
                .setResponseCode(200)
                .setBody("""{"kind":"lesson","lesson_id":"c-01","reasons":["Next in sequence"]}"""),
        )
        server.enqueue(
            MockResponse()
                .setResponseCode(200)
                .setBody("""{"ok":true}"""),
        )

        val repo = HomeRepository(createApi(token = "secret-token-123"))

        val nextRes = repo.fetchLearnNext("rust systems/101")
        assertTrue(nextRes is ApiResult.Ok)
        assertEquals("c-01", nextRes.getOrNull()?.lessonId)

        val playRes = repo.playStagedMusic("spotify:track:123")
        assertTrue(playRes is ApiResult.Ok)
        assertEquals(true, playRes.getOrNull()?.ok)

        val firstReq = server.takeRequest()
        assertEquals("GET", firstReq.method)
        assertEquals("/api/learn/next?course=rust+systems%2F101", firstReq.path)
        assertEquals("Bearer secret-token-123", firstReq.getHeader("Authorization"))

        val secondReq = server.takeRequest()
        assertEquals("POST", secondReq.method)
        assertEquals("/api/briefing/actions/play-music", secondReq.path)
        assertEquals("""{"uri":"spotify:track:123"}""", secondReq.body.readUtf8())
    }

    @Test
    fun viewModelBuildsReadyDashboardFromFixtures() = runTest {
        val briefingJson = readFixture("fixtures/home/briefing_latest.json")
        val metricsJson = readFixture("fixtures/home/system_metrics.json")
        val coursesJson = readFixture("fixtures/home/learn_courses.json")
        val spotifyJson = readFixture("fixtures/home/spotify_status.json")

        server.dispatcher = object : Dispatcher() {
            override fun dispatch(request: RecordedRequest): MockResponse =
                when (request.path) {
                    "/api/briefing/latest" -> MockResponse().setResponseCode(200).setBody(briefingJson)
                    "/api/system/metrics" -> MockResponse().setResponseCode(200).setBody(metricsJson)
                    "/api/learn/courses" -> MockResponse().setResponseCode(200).setBody(coursesJson)
                    "/api/spotify/status" -> MockResponse().setResponseCode(200).setBody(spotifyJson)
                    "/api/health" -> MockResponse().setResponseCode(200).setBody("""{"speech":{},"llm":{}}""")
                    "/api/mind/status" -> MockResponse().setResponseCode(200).setBody("""{"version":1,"accepting_requests":true,"models":[]}""")
                    "/api/briefing/actions/dismiss" -> MockResponse().setResponseCode(200).setBody("""{"dismissed":true}""")
                    else -> MockResponse().setResponseCode(404).setBody("""{"error":"not found"}""")
                }
        }

        val viewModel = HomeViewModel(HomeRepository(createApi()))
        val current = viewModel.state.value
        assertTrue(current is UiState.Ready)

        val dashboard = (current as UiState.Ready).value
        assertEquals("Welcome back.", dashboard.greeting)
        assertEquals("Here's your return briefing.", dashboard.headline)
        assertEquals("Avril 14th", dashboard.music.title)
        assertTrue(dashboard.music.isPlaying)
        assertEquals("Rust Systems Programming", dashboard.arena.title)
        assertEquals("rust-systems", dashboard.arena.courseId)
        assertEquals("rust-systems-02", dashboard.arena.nextLessonId)
        assertEquals(3, dashboard.arena.dueCount)
        assertEquals("SAIL", dashboard.system.workspaceName)
        assertEquals("All core systems nominal.", dashboard.system.statusSummary)
        assertEquals("76.9 GB free", dashboard.system.storageLabel)
        assertEquals("2.0 / 30.0 GB", dashboard.system.memoryLabel)
        assertEquals(1, dashboard.eventsToWatch.size)
        assertEquals(1, dashboard.newsRadar.size)

        viewModel.dismissBriefing()
        val afterDismiss = (viewModel.state.value as UiState.Ready).value
        assertTrue(afterDismiss.briefingDismissed)
        assertTrue(afterDismiss.eventsToWatch.isEmpty())
        assertTrue(afterDismiss.newsRadar.isEmpty())
    }

    @Test
    fun viewModelShowsHonestEmptyFallbacksAndUnavailableWhenAllFail() = runTest {
        val musicFallback = HomeViewModel.buildMusicCard(briefing = null, spotify = null)
        assertEquals("No music staged", musicFallback.title)
        assertEquals("History is empty or unavailable.", musicFallback.subtitle)

        val arenaUnavailable = HomeViewModel.buildArenaCard(
            briefing = null,
            course = null,
            next = null,
            coursesAvailable = false,
        )
        assertEquals("Resume in Arena", arenaUnavailable.title)
        assertEquals("Courses unavailable right now.", arenaUnavailable.subtitle)

        server.dispatcher = object : Dispatcher() {
            override fun dispatch(request: RecordedRequest): MockResponse =
                MockResponse().setResponseCode(401).setBody("""{"error":"Unauthorized"}""")
        }

        val viewModel = HomeViewModel(HomeRepository(createApi()))
        val state = viewModel.state.value
        assertTrue(state is UiState.Unavailable)
        val unavailable = state as UiState.Unavailable
        assertFalse(unavailable.message.isBlank())
    }
}
