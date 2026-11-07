package dev.sail.app.feature.tracker

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import dev.sail.app.core.UiState
import dev.sail.app.core.net.ApiResult
import kotlin.math.abs
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch

data class TrackerAnomaliesCardModel(
    val activeCount: Int,
    val countLabel: String,
    val topSigmaLabel: String? = null,
    val topHeadline: String,
    val items: List<TrackerAnomaly> = emptyList(),
)

data class TrackerMarketsCardModel(
    val headline: String,
    val changePctLabel: String? = null,
    val isPositive: Boolean = true,
    val subtitle: String,
    val quotes: List<TrackerQuote> = emptyList(),
    val fx: List<TrackerFxRate> = emptyList(),
    val odds: List<TrackerOddsMarket> = emptyList(),
)

data class TrackerMapPin(
    val label: String,
    val primary: Boolean,
    val point: NormalizedPoint,
    val tempC: Double? = null,
    val condition: String? = null,
)

data class TrackerMapRoute(
    val label: String,
    val points: List<NormalizedPoint>,
)

data class TrackerMapQuake(
    val id: String,
    val magnitude: Double,
    val place: String,
    val point: NormalizedPoint,
)

data class TrackerMapCardModel(
    val viewport: MapViewport,
    val tileUrls: List<String?>,
    val pins: List<TrackerMapPin>,
    val routes: List<TrackerMapRoute>,
    val quakes: List<TrackerMapQuake>,
    val aircraftPoints: List<NormalizedPoint>,
    val airborneTotal: Int,
)

data class TrackerDashboard(
    val clockSubtitle: String,
    val isLive: Boolean,
    val anomaliesCard: TrackerAnomaliesCardModel,
    val marketsCard: TrackerMarketsCardModel,
    val mapCard: TrackerMapCardModel,
    val topics: List<String>,
    val selectedTopic: String?,
    val stories: List<TrackerStory>,
    val filteredStories: List<TrackerStory>,
    val conditions: List<TrackerConditions>,
    val quakes: List<TrackerQuake>,
    val sourcesHealthyCount: Int,
    val sourcesTotalCount: Int,
)

class TrackerViewModel(
    private val repository: TrackerRepository,
) : ViewModel() {

    private val _state = MutableStateFlow<UiState<TrackerDashboard>>(UiState.Loading)
    val state: StateFlow<UiState<TrackerDashboard>> = _state.asStateFlow()

    private val _isRefreshing = MutableStateFlow(false)
    val isRefreshing: StateFlow<Boolean> = _isRefreshing.asStateFlow()

    private var selectedTopicFilter: String? = null

    init {
        load()
    }

    fun load() {
        viewModelScope.launch {
            if (_state.value !is UiState.Ready) {
                _state.value = UiState.Loading
            }
            _isRefreshing.value = true
            _state.value = mapEnvelopeResult(repository.fetchBoard())
            _isRefreshing.value = false
        }
    }

    fun refreshNow(source: String? = null) {
        viewModelScope.launch {
            _isRefreshing.value = true
            val result = repository.refresh(source = source)
            if (result is ApiResult.Ok) {
                _state.value = mapEnvelopeResult(result)
            } else {
                val fallback = repository.fetchBoard()
                _state.value = mapEnvelopeResult(fallback)
            }
            _isRefreshing.value = false
        }
    }

    fun selectTopic(topic: String?) {
        val normalized = topic?.takeIf { it.isNotBlank() }
        selectedTopicFilter = if (selectedTopicFilter.equals(normalized, ignoreCase = true)) {
            null
        } else {
            normalized
        }
        val current = (_state.value as? UiState.Ready)?.value ?: return
        val filtered = filterStories(current.stories, selectedTopicFilter)
        _state.value = UiState.Ready(
            current.copy(
                selectedTopic = selectedTopicFilter,
                filteredStories = filtered,
            ),
        )
    }

    internal fun mapEnvelopeResult(result: ApiResult<TrackerBoardEnvelope>): UiState<TrackerDashboard> =
        when (result) {
            is ApiResult.Err -> UiState.Unavailable(result.message)
            is ApiResult.Ok -> {
                val envelope = result.value
                val dashboard = buildDashboard(
                    envelope = envelope,
                    selectedTopic = selectedTopicFilter,
                    tileUrlProvider = { z, x, y -> repository.tileUrl(z, x, y) },
                )
                UiState.Ready(dashboard)
            }
        }

    companion object {
        internal fun filterStories(
            stories: List<TrackerStory>,
            topic: String?,
        ): List<TrackerStory> {
            val clean = topic?.trim()?.takeIf { it.isNotEmpty() } ?: return stories
            return stories.filter { it.topic.equals(clean, ignoreCase = true) }
        }

        internal fun buildDashboard(
            envelope: TrackerBoardEnvelope,
            selectedTopic: String? = null,
            tileUrlProvider: (Int, Int, Int) -> String? = { _, _, _ -> null },
        ): TrackerDashboard {
            val board = envelope.board
            val config = envelope.config

            val anomalies = board.anomalies.sortedByDescending { abs(it.zScore) }
            val topAnomaly = anomalies.firstOrNull()
            val anomaliesCard = TrackerAnomaliesCardModel(
                activeCount = anomalies.size,
                countLabel = "${anomalies.size} active",
                topSigmaLabel = topAnomaly?.let { TrackerFormatting.formatSigma(it.zScore) },
                topHeadline = topAnomaly?.headline?.takeIf { it.isNotBlank() }
                    ?: "No active anomalies across monitored feeds.",
                items = anomalies,
            )

            val primaryQuote = board.quotes.firstOrNull {
                it.symbol == "^GSPC" || it.label.startsWith("S&P", ignoreCase = true)
            } ?: board.quotes.firstOrNull()

            val fxSample = board.fx.firstOrNull()
            val marketSubtitle = when {
                fxSample != null -> {
                    val rateStr = TrackerFormatting.formatQuotePrice(fxSample.rate)
                    "${fxSample.base}/${fxSample.quote} $rateStr · ${board.quotes.size} quotes"
                }
                primaryQuote != null && primaryQuote.label.isNotBlank() -> primaryQuote.label
                else -> "No live quotes available."
            }

            val marketsCard = TrackerMarketsCardModel(
                headline = if (primaryQuote != null) {
                    TrackerFormatting.formatQuoteHeadline(primaryQuote)
                } else {
                    "No quotes"
                },
                changePctLabel = primaryQuote?.let {
                    TrackerFormatting.formatPercentChange(it.changePct)
                },
                isPositive = (primaryQuote?.changePct ?: 0.0) >= 0.0,
                subtitle = marketSubtitle,
                quotes = board.quotes,
                fx = board.fx,
                odds = board.odds,
            )

            val places = if (config.places.isNotEmpty()) {
                config.places
            } else {
                board.conditions.map { cond ->
                    TrackerPlace(
                        label = cond.place,
                        lat = cond.lat,
                        lon = cond.lon,
                        tz = cond.tz,
                        primary = false,
                    )
                }
            }

            val viewport = WebMercator.buildViewport(places = places, cols = 3, rows = 2)
            val tileUrls = viewport.tiles.map { coord ->
                tileUrlProvider(coord.z, coord.x, coord.y)
            }

            val conditionByPlace = board.conditions.associateBy { it.place.lowercase() }
            val pins = places.map { place ->
                val cond = conditionByPlace[place.label.lowercase()]
                TrackerMapPin(
                    label = place.label,
                    primary = place.primary,
                    point = viewport.project(place.lat, place.lon),
                    tempC = cond?.tempC,
                    condition = cond?.condition?.takeIf { it.isNotBlank() },
                )
            }

            val routes = board.routes.map { route ->
                val pts = if (route.geometry.size >= 2) {
                    route.geometry.mapNotNull { pair ->
                        if (pair.size >= 2) {
                            viewport.project(lat = pair[1], lon = pair[0])
                        } else {
                            null
                        }
                    }
                } else {
                    listOf(
                        viewport.project(lat = route.fromLat, lon = route.fromLon),
                        viewport.project(lat = route.toLat, lon = route.toLon),
                    )
                }
                TrackerMapRoute(
                    label = route.label,
                    points = pts,
                )
            }

            val quakeMarkers = board.quakes.take(12).map { quake ->
                TrackerMapQuake(
                    id = quake.id,
                    magnitude = quake.magnitude,
                    place = quake.place,
                    point = viewport.project(lat = quake.lat, lon = quake.lon),
                )
            }

            val aircraftPoints = board.air
                .flatMap { it.sample }
                .take(40)
                .map { ac -> viewport.project(lat = ac.lat, lon = ac.lon) }

            val airborneTotal = board.air.sumOf { it.airborne }

            val mapCard = TrackerMapCardModel(
                viewport = viewport,
                tileUrls = tileUrls,
                pins = pins,
                routes = routes,
                quakes = quakeMarkers,
                aircraftPoints = aircraftPoints,
                airborneTotal = airborneTotal,
            )

            val discoveredTopics = buildList {
                addAll(config.topics)
                board.topics.forEach { add(it.topic) }
                board.news.forEach { add(it.topic) }
            }
                .map { it.trim() }
                .filter { it.isNotEmpty() }
                .distinctBy { it.lowercase() }

            val stories = board.news
            val filteredStories = filterStories(stories, selectedTopic)

            val totalSources = board.sources.size
            val healthySources = board.sources.values.count { it.ok }
            val isLive = healthySources > 0 ||
                stories.isNotEmpty() ||
                board.quotes.isNotEmpty() ||
                anomalies.isNotEmpty()

            return TrackerDashboard(
                clockSubtitle = TrackerFormatting.formatUtcClock(envelope.serverTime),
                isLive = isLive,
                anomaliesCard = anomaliesCard,
                marketsCard = marketsCard,
                mapCard = mapCard,
                topics = discoveredTopics,
                selectedTopic = selectedTopic,
                stories = stories,
                filteredStories = filteredStories,
                conditions = board.conditions,
                quakes = board.quakes,
                sourcesHealthyCount = healthySources,
                sourcesTotalCount = totalSources,
            )
        }
    }
}
