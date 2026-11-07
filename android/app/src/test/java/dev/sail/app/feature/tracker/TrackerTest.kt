package dev.sail.app.feature.tracker

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
import okhttp3.mockwebserver.MockResponse
import okhttp3.mockwebserver.MockWebServer
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test

@OptIn(ExperimentalCoroutinesApi::class)
class TrackerTest {

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

    private fun createApi(token: String = "tracker-owner-token"): SailApi {
        val baseUrl = server.url("/")
        return SailApi(config = { ServerConfig(baseUrl = baseUrl, token = token) })
    }

    @Test
    fun parsesTrackerBoardFixtureAndFormatsValues() {
        val json = SailApi.defaultJson
        val envelope = json.decodeFromString(
            TrackerBoardEnvelope.serializer(),
            readFixture("fixtures/tracker/tracker_board.json"),
        )

        assertEquals(3, envelope.config.places.size)
        assertEquals(6, envelope.board.anomalies.size)
        assertEquals(2, envelope.board.quotes.size)
        assertEquals(2, envelope.board.news.size)
        assertEquals(3, envelope.board.news.first().also.size)

        assertEquals("+2.4σ", TrackerFormatting.formatSigma(2.4))
        assertEquals("-1.3σ", TrackerFormatting.formatSigma(-1.3))
        assertEquals("+1.2%", TrackerFormatting.formatPercentChange(1.21))
        assertEquals("-0.8%", TrackerFormatting.formatPercentChange(-0.82))
        assertEquals("5,842", TrackerFormatting.formatQuotePrice(5842.47))
        assertEquals("1.0842", TrackerFormatting.formatQuotePrice(1.0842))
        assertEquals("S&P 5,842", TrackerFormatting.formatQuoteHeadline(envelope.board.quotes.first()))
        assertEquals("14:18 UTC · 14:18Z", TrackerFormatting.formatUtcClock(1730989080L))
        assertEquals("14:02", TrackerFormatting.formatStoryTime(1730988120L))
    }

    @Test
    fun projectsWebMercatorViewportAndPinsWithinNormalizedBounds() {
        val places = listOf(
            TrackerPlace(label = "Dublin", lat = 53.3498, lon = -6.2603, primary = true),
            TrackerPlace(label = "London", lat = 51.5072, lon = -0.1276),
            TrackerPlace(label = "New York", lat = 40.7128, lon = -74.0060),
        )

        val viewport = WebMercator.buildViewport(places = places, cols = 3, rows = 2)
        assertEquals(6, viewport.tiles.size)
        assertTrue(viewport.zoom in 2..6)

        val dublinPt = viewport.project(53.3498, -6.2603)
        val nyPt = viewport.project(40.7128, -74.0060)

        assertTrue(dublinPt.x in 0.02f..0.98f)
        assertTrue(dublinPt.y in 0.04f..0.96f)
        assertTrue(nyPt.x in 0.02f..0.98f)
        assertTrue(nyPt.y in 0.04f..0.96f)
        assertTrue("New York is west of Dublin", nyPt.x < dublinPt.x)
    }

    @Test
    fun repositoryFetchesBoardRefreshesSourceAndResolvesTileUrl() = runTest {
        val fixture = readFixture("fixtures/tracker/tracker_board.json")
        server.enqueue(MockResponse().setResponseCode(200).setBody(fixture))
        server.enqueue(MockResponse().setResponseCode(200).setBody(fixture))

        val repo = TrackerRepository(createApi(token = "token-abc"))

        val boardRes = repo.fetchBoard()
        assertTrue(boardRes is ApiResult.Ok)
        assertEquals(6, boardRes.getOrNull()?.board?.anomalies?.size)

        val refreshRes = repo.refresh(source = "markets")
        assertTrue(refreshRes is ApiResult.Ok)

        val tileUrl = repo.tileUrl(z = 3, x = 2, y = 1)
        assertNotNull(tileUrl)
        assertTrue(tileUrl?.endsWith("/api/tracker/tiles/3/2/1.png") == true)

        val req1 = server.takeRequest()
        assertEquals("GET", req1.method)
        assertEquals("/api/tracker/board", req1.path)
        assertEquals("Bearer token-abc", req1.getHeader("Authorization"))

        val req2 = server.takeRequest()
        assertEquals("POST", req2.method)
        assertEquals("/api/tracker/refresh", req2.path)
        assertEquals("""{"source":"markets"}""", req2.body.readUtf8())
    }

    @Test
    fun viewModelMapsSituationRoomStateAndFiltersNewsByTopic() = runTest {
        val fixture = readFixture("fixtures/tracker/tracker_board.json")
        server.enqueue(MockResponse().setResponseCode(200).setBody(fixture))

        val viewModel = TrackerViewModel(TrackerRepository(createApi()))
        val state = viewModel.state.value
        assertTrue(state is UiState.Ready)

        val dashboard = (state as UiState.Ready).value
        assertTrue(dashboard.isLive)
        assertEquals("14:18 UTC · 14:18Z", dashboard.clockSubtitle)
        assertEquals(6, dashboard.anomaliesCard.activeCount)
        assertEquals("6 active", dashboard.anomaliesCard.countLabel)
        assertEquals("+2.4σ", dashboard.anomaliesCard.topSigmaLabel)
        assertEquals("S&P 500 +2.4σ move", dashboard.anomaliesCard.topHeadline)
        assertEquals("S&P 5,842", dashboard.marketsCard.headline)
        assertEquals("+1.2%", dashboard.marketsCard.changePctLabel)
        assertTrue(dashboard.marketsCard.isPositive)
        assertEquals(3, dashboard.mapCard.pins.size)
        assertEquals(6, dashboard.mapCard.tileUrls.size)
        assertEquals(38, dashboard.mapCard.airborneTotal)
        assertEquals(2, dashboard.filteredStories.size)

        viewModel.selectTopic("technology")
        val techFiltered = (viewModel.state.value as UiState.Ready).value
        assertEquals("technology", techFiltered.selectedTopic)
        assertEquals(1, techFiltered.filteredStories.size)
        assertEquals("Ars Technica", techFiltered.filteredStories.first().source)

        viewModel.selectTopic("technology")
        val cleared = (viewModel.state.value as UiState.Ready).value
        assertNull(cleared.selectedTopic)
        assertEquals(2, cleared.filteredStories.size)
    }

    @Test
    fun viewModelExposesUnavailableOnServerFailure() = runTest {
        server.enqueue(MockResponse().setResponseCode(503).setBody("""{"error":"Tracker offline"}"""))

        val viewModel = TrackerViewModel(TrackerRepository(createApi()))
        val state = viewModel.state.value
        assertTrue(state is UiState.Unavailable)
        assertEquals("Tracker offline", (state as UiState.Unavailable).message)
    }
}
