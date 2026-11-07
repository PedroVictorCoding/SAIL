package dev.sail.app.feature.tracker

import java.text.NumberFormat
import java.time.Instant
import java.time.ZoneOffset
import java.time.format.DateTimeFormatter
import java.util.Locale
import kotlin.math.PI
import kotlin.math.abs
import kotlin.math.atan
import kotlin.math.cos
import kotlin.math.floor
import kotlin.math.ln
import kotlin.math.max
import kotlin.math.min
import kotlin.math.pow
import kotlin.math.roundToInt
import kotlin.math.sinh
import kotlin.math.tan
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable

@Serializable
data class TrackerBoardEnvelope(
    val config: TrackerConfig = TrackerConfig(),
    val board: TrackerBoard = TrackerBoard(),
    @SerialName("server_time")
    val serverTime: Long = 0L,
)

@Serializable
data class TrackerConfig(
    val places: List<TrackerPlace> = emptyList(),
    val tickers: List<TrackerTicker> = emptyList(),
    val topics: List<String> = emptyList(),
    val feeds: List<TrackerFeed> = emptyList(),
    val routes: List<TrackerRouteConfig> = emptyList(),
    val markets: List<String> = emptyList(),
    @SerialName("base_currency")
    val baseCurrency: String = "EUR",
    val currencies: List<String> = emptyList(),
    val pinned: List<String> = emptyList(),
)

@Serializable
data class TrackerPlace(
    val label: String,
    val lat: Double,
    val lon: Double,
    val tz: String = "UTC",
    val primary: Boolean = false,
)

@Serializable
data class TrackerTicker(
    val symbol: String,
    val label: String,
    val kind: String = "equity",
)

@Serializable
data class TrackerFeed(
    val name: String,
    val url: String,
    val topic: String = "world",
    val lang: String = "en",
)

@Serializable
data class TrackerRouteConfig(
    val label: String,
    val from: String = "",
    val to: String = "",
    @SerialName("from_lat")
    val fromLat: Double = 0.0,
    @SerialName("from_lon")
    val fromLon: Double = 0.0,
    @SerialName("to_lat")
    val toLat: Double = 0.0,
    @SerialName("to_lon")
    val toLon: Double = 0.0,
    val mode: String = "driving",
)

@Serializable
data class TrackerBoard(
    val news: List<TrackerStory> = emptyList(),
    val topics: List<TrackerTopicVolume> = emptyList(),
    val quotes: List<TrackerQuote> = emptyList(),
    val fx: List<TrackerFxRate> = emptyList(),
    val odds: List<TrackerOddsMarket> = emptyList(),
    val conditions: List<TrackerConditions> = emptyList(),
    val air: List<TrackerAirTraffic> = emptyList(),
    val routes: List<TrackerRouteInfo> = emptyList(),
    val quakes: List<TrackerQuake> = emptyList(),
    val anomalies: List<TrackerAnomaly> = emptyList(),
    val sources: Map<String, TrackerSourceStatus> = emptyMap(),
)

@Serializable
data class TrackerStory(
    val id: String = "",
    val title: String,
    val url: String = "",
    val source: String = "",
    val topic: String = "world",
    val published: Long? = null,
    val summary: String? = null,
    val also: List<TrackerStoryRef> = emptyList(),
)

@Serializable
data class TrackerStoryRef(
    val source: String = "",
    val url: String = "",
    val title: String = "",
)

@Serializable
data class TrackerTopicVolume(
    val topic: String,
    @SerialName("count_1h")
    val count1h: Int = 0,
    @SerialName("count_24h")
    val count24h: Int = 0,
    val sparkline: List<Int> = emptyList(),
)

@Serializable
data class TrackerQuote(
    val symbol: String,
    val label: String = "",
    val kind: String = "equity",
    val price: Double = 0.0,
    val change: Double = 0.0,
    @SerialName("change_pct")
    val changePct: Double = 0.0,
    val currency: String? = null,
    @SerialName("market_state")
    val marketState: String? = null,
    val sparkline: List<Double> = emptyList(),
    @SerialName("high_24h")
    val high24h: Double? = null,
    @SerialName("low_24h")
    val low24h: Double? = null,
    val volume: Double? = null,
    @SerialName("updated_at")
    val updatedAt: Long = 0L,
)

@Serializable
data class TrackerFxRate(
    val base: String,
    val quote: String,
    val rate: Double,
    @SerialName("change_pct")
    val changePct: Double? = null,
    val sparkline: List<Double> = emptyList(),
    val date: String = "",
)

@Serializable
data class TrackerOddsMarket(
    val id: String = "",
    val question: String,
    val category: String = "",
    val probability: Double = 0.0,
    @SerialName("delta_24h")
    val delta24h: Double? = null,
    @SerialName("volume_usd")
    val volumeUsd: Double? = null,
    @SerialName("closes_at")
    val closesAt: String? = null,
    val url: String? = null,
    val outcomes: List<TrackerOddsOutcome> = emptyList(),
)

@Serializable
data class TrackerOddsOutcome(
    val label: String,
    val probability: Double = 0.0,
)

@Serializable
data class TrackerConditions(
    val place: String,
    val lat: Double = 0.0,
    val lon: Double = 0.0,
    val tz: String = "UTC",
    @SerialName("temp_c")
    val tempC: Double = 0.0,
    @SerialName("feels_c")
    val feelsC: Double = 0.0,
    @SerialName("wind_kph")
    val windKph: Double = 0.0,
    @SerialName("wind_dir")
    val windDir: Int = 0,
    val humidity: Double = 0.0,
    @SerialName("precip_mm")
    val precipMm: Double = 0.0,
    val condition: String = "",
    @SerialName("wmo_code")
    val wmoCode: Int = 0,
    @SerialName("is_day")
    val isDay: Boolean = true,
    @SerialName("aqi_eu")
    val aqiEu: Int? = null,
    val pm25: Double? = null,
    val pm10: Double? = null,
    @SerialName("uv_index")
    val uvIndex: Double? = null,
    @SerialName("hourly_temp")
    val hourlyTemp: List<Double> = emptyList(),
    @SerialName("updated_at")
    val updatedAt: Long = 0L,
)

@Serializable
data class TrackerAirTraffic(
    val place: String,
    val lat: Double = 0.0,
    val lon: Double = 0.0,
    @SerialName("radius_km")
    val radiusKm: Double = 250.0,
    val total: Int = 0,
    val airborne: Int = 0,
    val sample: List<TrackerAircraft> = emptyList(),
    @SerialName("updated_at")
    val updatedAt: Long = 0L,
)

@Serializable
data class TrackerAircraft(
    val icao24: String = "",
    val callsign: String = "",
    val country: String = "",
    val lat: Double,
    val lon: Double,
    @SerialName("alt_m")
    val altM: Double? = null,
    @SerialName("vel_ms")
    val velMs: Double? = null,
    val heading: Double? = null,
)

@Serializable
data class TrackerRouteInfo(
    val label: String,
    val mode: String = "driving",
    @SerialName("distance_km")
    val distanceKm: Double = 0.0,
    @SerialName("duration_min")
    val durationMin: Double = 0.0,
    @SerialName("from_lat")
    val fromLat: Double = 0.0,
    @SerialName("from_lon")
    val fromLon: Double = 0.0,
    @SerialName("to_lat")
    val toLat: Double = 0.0,
    @SerialName("to_lon")
    val toLon: Double = 0.0,
    val geometry: List<List<Double>> = emptyList(),
)

@Serializable
data class TrackerQuake(
    val id: String = "",
    val magnitude: Double = 0.0,
    val place: String = "",
    val time: Long = 0L,
    val lat: Double = 0.0,
    val lon: Double = 0.0,
    @SerialName("depth_km")
    val depthKm: Double = 0.0,
    val url: String? = null,
    val tsunami: Boolean = false,
)

@Serializable
data class TrackerAnomaly(
    val id: String = "",
    val kind: String = "",
    val subject: String = "",
    val headline: String = "",
    val detail: String = "",
    @SerialName("z_score")
    val zScore: Double = 0.0,
    val severity: String = "info",
    @SerialName("detected_at")
    val detectedAt: Long = 0L,
)

@Serializable
data class TrackerSourceStatus(
    val source: String = "",
    val ok: Boolean = false,
    @SerialName("updated_at")
    val updatedAt: Long? = null,
    @SerialName("item_count")
    val itemCount: Int = 0,
    val error: String? = null,
    @SerialName("latency_ms")
    val latencyMs: Long = 0L,
)

@Serializable
data class TrackerRefreshRequest(
    val source: String? = null,
)

data class MapTileCoord(
    val z: Int,
    val x: Int,
    val y: Int,
)

data class NormalizedPoint(
    val x: Float,
    val y: Float,
)

data class MapViewport(
    val zoom: Int,
    val minTileX: Int,
    val minTileY: Int,
    val cols: Int,
    val rows: Int,
    val tiles: List<MapTileCoord>,
) {
    fun project(lat: Double, lon: Double): NormalizedPoint {
        val worldX = WebMercator.lonToTileX(lon, zoom)
        val worldY = WebMercator.latToTileY(lat, zoom)
        val normX = ((worldX - minTileX) / cols.coerceAtLeast(1)).toFloat()
        val normY = ((worldY - minTileY) / rows.coerceAtLeast(1)).toFloat()
        return NormalizedPoint(
            x = normX.coerceIn(0.02f, 0.98f),
            y = normY.coerceIn(0.04f, 0.96f),
        )
    }
}

object WebMercator {
    private const val MAX_LAT = 85.05112878

    fun lonToTileX(lon: Double, zoom: Int): Double {
        val clampedLon = lon.coerceIn(-180.0, 180.0)
        val n = 2.0.pow(zoom.coerceIn(0, 18))
        return ((clampedLon + 180.0) / 360.0) * n
    }

    fun latToTileY(lat: Double, zoom: Int): Double {
        val clampedLat = lat.coerceIn(-MAX_LAT, MAX_LAT)
        val rad = clampedLat * PI / 180.0
        val n = 2.0.pow(zoom.coerceIn(0, 18))
        return (1.0 - ln(tan(rad) + 1.0 / cos(rad)) / PI) / 2.0 * n
    }

    fun tileYToLat(tileY: Double, zoom: Int): Double {
        val n = 2.0.pow(zoom.coerceIn(0, 18))
        val m = PI - (2.0 * PI * tileY) / n
        return 180.0 / PI * atan(sinh(m))
    }

    fun buildViewport(
        places: List<TrackerPlace>,
        cols: Int = 3,
        rows: Int = 2,
    ): MapViewport {
        val safeCols = cols.coerceIn(1, 4)
        val safeRows = rows.coerceIn(1, 3)

        if (places.isEmpty()) {
            val z = 2
            val minX = 1
            val minY = 1
            return createGrid(z = z, minX = minX, minY = minY, cols = safeCols, rows = safeRows)
        }

        val minLat = places.minOf { it.lat }
        val maxLat = places.maxOf { it.lat }
        val minLon = places.minOf { it.lon }
        val maxLon = places.maxOf { it.lon }

        var chosenZoom = 2
        for (z in 6 downTo 2) {
            val xSpan = abs(lonToTileX(maxLon, z) - lonToTileX(minLon, z))
            val ySpan = abs(latToTileY(minLat, z) - latToTileY(maxLat, z))
            if (xSpan <= safeCols * 0.72 && ySpan <= safeRows * 0.72) {
                chosenZoom = z
                break
            }
        }

        val centerLon = (minLon + maxLon) / 2.0
        val centerLat = (minLat + maxLat) / 2.0
        val maxTileIndex = (1 shl chosenZoom) - 1

        val centerTileX = lonToTileX(centerLon, chosenZoom)
        val centerTileY = latToTileY(centerLat, chosenZoom)

        val rawMinX = floor(centerTileX - safeCols / 2.0).toInt()
        val rawMinY = floor(centerTileY - safeRows / 2.0).toInt()

        val clampedMinX = rawMinX.coerceIn(0, max(0, maxTileIndex - safeCols + 1))
        val clampedMinY = rawMinY.coerceIn(0, max(0, maxTileIndex - safeRows + 1))

        return createGrid(
            z = chosenZoom,
            minX = clampedMinX,
            minY = clampedMinY,
            cols = safeCols,
            rows = safeRows,
        )
    }

    private fun createGrid(
        z: Int,
        minX: Int,
        minY: Int,
        cols: Int,
        rows: Int,
    ): MapViewport {
        val maxTileIndex = (1 shl z) - 1
        val tiles = buildList {
            for (dy in 0 until rows) {
                for (dx in 0 until cols) {
                    val x = min(maxTileIndex, max(0, minX + dx))
                    val y = min(maxTileIndex, max(0, minY + dy))
                    add(MapTileCoord(z = z, x = x, y = y))
                }
            }
        }
        return MapViewport(
            zoom = z,
            minTileX = minX,
            minTileY = minY,
            cols = cols,
            rows = rows,
            tiles = tiles,
        )
    }
}

object TrackerFormatting {
    private val utcClockFormatter: DateTimeFormatter =
        DateTimeFormatter.ofPattern("HH:mm", Locale.US).withZone(ZoneOffset.UTC)

    fun formatSigma(zScore: Double): String {
        if (!zScore.isFinite()) return "0.0σ"
        val sign = if (zScore >= 0.0) "+" else ""
        return String.format(Locale.US, "%s%.1fσ", sign, zScore)
    }

    fun formatPercentChange(pct: Double): String {
        if (!pct.isFinite()) return "0.0%"
        val sign = if (pct >= 0.0) "+" else ""
        return String.format(Locale.US, "%s%.1f%%", sign, pct)
    }

    fun formatQuotePrice(price: Double): String {
        if (!price.isFinite()) return "—"
        return when {
            abs(price) >= 100.0 -> {
                val formatter = NumberFormat.getIntegerInstance(Locale.US)
                formatter.format(price.roundToInt())
            }
            abs(price) >= 1.0 -> String.format(Locale.US, "%.2f", price)
            else -> String.format(Locale.US, "%.4f", price)
        }
    }

    fun formatFxRate(rate: Double): String {
        if (!rate.isFinite()) return "—"
        return String.format(Locale.US, "%.4f", rate)
    }

    fun formatQuoteHeadline(quote: TrackerQuote?): String {
        if (quote == null) return "Markets"
        val shortName = when {
            quote.symbol == "^GSPC" || quote.label.startsWith("S&P", ignoreCase = true) -> "S&P"
            quote.label.isNotBlank() -> quote.label.split(" ").first()
            else -> quote.symbol
        }
        return "$shortName ${formatQuotePrice(quote.price)}"
    }

    fun formatUtcClock(epochSeconds: Long): String {
        if (epochSeconds <= 0L) return "UTC"
        val timeStr = utcClockFormatter.format(Instant.ofEpochSecond(epochSeconds))
        return "$timeStr UTC · ${timeStr}Z"
    }

    fun formatStoryTime(publishedEpochSeconds: Long?): String {
        val secs = publishedEpochSeconds?.takeIf { it > 0L } ?: return "—"
        return utcClockFormatter.format(Instant.ofEpochSecond(secs))
    }
}
