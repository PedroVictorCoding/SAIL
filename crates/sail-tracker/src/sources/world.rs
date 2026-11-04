//! Place-based sources: weather and air quality (Open-Meteo), air traffic (OpenSky),
//! road routes (OSRM), earthquakes (USGS) and place search (Open-Meteo geocoding).
use crate::config::Place;
use serde::Serialize;
use serde_json::Value;

pub fn haversine_km(a: (f64, f64), b: (f64, f64)) -> f64 {
    let (lat1, lon1) = (a.0.to_radians(), a.1.to_radians());
    let (lat2, lon2) = (b.0.to_radians(), b.1.to_radians());
    let h = ((lat2 - lat1) / 2.0).sin().powi(2)
        + lat1.cos() * lat2.cos() * ((lon2 - lon1) / 2.0).sin().powi(2);
    2.0 * 6371.0 * h.sqrt().asin()
}

fn coords(places: &[Place]) -> (String, String) {
    let lat = places
        .iter()
        .map(|p| format!("{:.4}", p.lat))
        .collect::<Vec<_>>()
        .join(",");
    let lon = places
        .iter()
        .map(|p| format!("{:.4}", p.lon))
        .collect::<Vec<_>>()
        .join(",");
    (lat, lon)
}

/// Open-Meteo returns an object for one location and an array for several.
fn per_location(body: &Value) -> Vec<&Value> {
    match body {
        Value::Array(a) => a.iter().collect(),
        Value::Object(_) => vec![body],
        _ => vec![],
    }
}

// ---------------------------------------------------------------- weather

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct HourPoint {
    pub time: String,
    pub temp: Option<f64>,
    pub precip_prob: Option<f64>,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Conditions {
    pub place: String,
    pub name: String,
    pub timezone: String,
    pub utc_offset_seconds: i64,
    pub local_time: String,
    pub temp: Option<f64>,
    pub feels_like: Option<f64>,
    pub humidity: Option<f64>,
    pub precipitation: Option<f64>,
    pub weather_code: Option<i64>,
    pub summary: String,
    pub wind: Option<f64>,
    pub gusts: Option<f64>,
    pub visibility_m: Option<f64>,
    pub cloud_cover: Option<f64>,
    pub pressure: Option<f64>,
    pub is_day: bool,
    /// Temperature minus the median at this hour over the past week.
    pub temp_vs_week: Option<f64>,
    pub temp_z: Option<f64>,
    pub gust_z: Option<f64>,
    pub aqi: Option<f64>,
    pub pm25: Option<f64>,
    pub next_hours: Vec<HourPoint>,
    /// A short look-ahead, such as "Rain likely in 3h (70%)".
    pub outlook: Option<String>,
}

pub fn weather_url(places: &[Place]) -> String {
    let (lat, lon) = coords(places);
    format!(
        "https://api.open-meteo.com/v1/forecast?latitude={lat}&longitude={lon}\
         &current=temperature_2m,apparent_temperature,relative_humidity_2m,precipitation,weather_code,\
wind_speed_10m,wind_gusts_10m,visibility,cloud_cover,pressure_msl,is_day\
         &hourly=temperature_2m,precipitation_probability,wind_gusts_10m\
         &past_days=7&forecast_days=2&timezone=auto"
    )
}

pub fn air_quality_url(places: &[Place]) -> String {
    let (lat, lon) = coords(places);
    format!(
        "https://air-quality-api.open-meteo.com/v1/air-quality?latitude={lat}&longitude={lon}\
         &current=us_aqi,pm2_5&timezone=auto"
    )
}

pub fn wmo_summary(code: i64) -> &'static str {
    match code {
        0 => "Clear",
        1 => "Mostly clear",
        2 => "Partly cloudy",
        3 => "Overcast",
        45 | 48 => "Fog",
        51 | 53 | 55 => "Drizzle",
        56 | 57 => "Freezing drizzle",
        61 => "Light rain",
        63 => "Rain",
        65 => "Heavy rain",
        66 | 67 => "Freezing rain",
        71 => "Light snow",
        73 => "Snow",
        75 => "Heavy snow",
        77 => "Snow grains",
        80 | 81 => "Showers",
        82 => "Violent showers",
        85 | 86 => "Snow showers",
        95 => "Thunderstorm",
        96 | 99 => "Thunderstorm, hail",
        _ => "Unknown",
    }
}

fn series(v: &Value, key: &str) -> Vec<Option<f64>> {
    v["hourly"][key]
        .as_array()
        .map(|a| a.iter().map(Value::as_f64).collect())
        .unwrap_or_default()
}

fn same_hour_last_week(values: &[Option<f64>], index: usize) -> Vec<f64> {
    (1..=7)
        .filter_map(|day| index.checked_sub(day * 24))
        .filter_map(|i| values.get(i).copied().flatten())
        .collect()
}

pub fn parse_weather(body: &Value, air: Option<&Value>, places: &[Place]) -> Vec<Conditions> {
    let air: Vec<&Value> = air.map(per_location).unwrap_or_default();
    per_location(body)
        .into_iter()
        .zip(places)
        .map(|(v, place)| {
            let cur = &v["current"];
            let local_time = cur["time"].as_str().unwrap_or("").to_string();
            let times: Vec<&str> = v["hourly"]["time"]
                .as_array()
                .map(|a| a.iter().filter_map(Value::as_str).collect())
                .unwrap_or_default();
            let hour_prefix = local_time.get(..13).unwrap_or("");
            let index = times.iter().position(|t| t.starts_with(hour_prefix));
            let temps = series(v, "temperature_2m");
            let probs = series(v, "precipitation_probability");
            let gusts = series(v, "wind_gusts_10m");
            let temp = cur["temperature_2m"].as_f64();
            let gust = cur["wind_gusts_10m"].as_f64();

            let (mut temp_vs_week, mut temp_z, mut gust_z) = (None, None, None);
            let mut next_hours = vec![];
            let mut outlook = None;
            if let Some(i) = index {
                if let (Some(t), Some(b)) = (
                    temp,
                    crate::anomaly::baseline(&same_hour_last_week(&temps, i), 5, 1.0),
                ) {
                    temp_vs_week = Some(t - b.center);
                    temp_z = Some(crate::anomaly::z(t, &b));
                }
                if let (Some(g), Some(b)) = (
                    gust,
                    crate::anomaly::baseline(&same_hour_last_week(&gusts, i), 5, 5.0),
                ) {
                    gust_z = Some(crate::anomaly::z(g, &b));
                }
                for h in 1..=12 {
                    let j = i + h;
                    let Some(time) = times.get(j) else { break };
                    next_hours.push(HourPoint {
                        time: time.to_string(),
                        temp: temps.get(j).copied().flatten(),
                        precip_prob: probs.get(j).copied().flatten(),
                    });
                }
                let now_wet = probs.get(i).copied().flatten().unwrap_or(0.0) >= 50.0;
                if let Some((h, p)) = next_hours
                    .iter()
                    .enumerate()
                    .find_map(|(h, p)| p.precip_prob.filter(|x| *x >= 50.0).map(|x| (h + 1, x)))
                    && !now_wet
                {
                    outlook = Some(format!("Rain likely in {h}h ({p:.0}%)"));
                } else if now_wet
                    && let Some(h) = next_hours
                        .iter()
                        .position(|p| p.precip_prob.is_some_and(|x| x < 30.0))
                {
                    outlook = Some(format!("Drying out in {}h", h + 1));
                }
            }
            let code = cur["weather_code"].as_i64();
            let aq = air.iter().find(|a| {
                (a["latitude"].as_f64().unwrap_or(999.0) - v["latitude"].as_f64().unwrap_or(0.0))
                    .abs()
                    < 0.5
                    && (a["longitude"].as_f64().unwrap_or(999.0)
                        - v["longitude"].as_f64().unwrap_or(0.0))
                    .abs()
                        < 0.5
            });
            Conditions {
                place: place.id.clone(),
                name: place.name.clone(),
                timezone: v["timezone"].as_str().unwrap_or("UTC").to_string(),
                utc_offset_seconds: v["utc_offset_seconds"].as_i64().unwrap_or(0),
                local_time,
                temp,
                feels_like: cur["apparent_temperature"].as_f64(),
                humidity: cur["relative_humidity_2m"].as_f64(),
                precipitation: cur["precipitation"].as_f64(),
                weather_code: code,
                summary: code.map(wmo_summary).unwrap_or("Unknown").to_string(),
                wind: cur["wind_speed_10m"].as_f64(),
                gusts: gust,
                visibility_m: cur["visibility"].as_f64(),
                cloud_cover: cur["cloud_cover"].as_f64(),
                pressure: cur["pressure_msl"].as_f64(),
                is_day: cur["is_day"].as_i64() == Some(1),
                temp_vs_week,
                temp_z,
                gust_z,
                aqi: aq.and_then(|a| a["current"]["us_aqi"].as_f64()),
                pm25: aq.and_then(|a| a["current"]["pm2_5"].as_f64()),
                next_hours,
                outlook,
            }
        })
        .collect()
}

// ---------------------------------------------------------------- air traffic

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Aircraft {
    pub callsign: String,
    pub country: String,
    pub lat: f64,
    pub lon: f64,
    pub altitude_m: Option<f64>,
    pub speed_kmh: Option<f64>,
    pub heading: Option<f64>,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct AirTraffic {
    pub place: String,
    pub name: String,
    pub airborne: usize,
    pub on_ground: usize,
    /// Median airborne count at this time of day on earlier days, once known.
    pub usual: Option<f64>,
    pub z: Option<f64>,
    pub aircraft: Vec<Aircraft>,
    pub radius_km: f64,
}

pub const AIR_RADIUS_KM: f64 = 60.0;

pub fn air_url(place: &Place) -> String {
    let dlat = AIR_RADIUS_KM / 111.0;
    let dlon = AIR_RADIUS_KM / (111.0 * place.lat.to_radians().cos().max(0.1));
    format!(
        "https://opensky-network.org/api/states/all?lamin={:.3}&lomin={:.3}&lamax={:.3}&lomax={:.3}",
        place.lat - dlat,
        place.lon - dlon,
        place.lat + dlat,
        place.lon + dlon
    )
}

pub fn parse_air(body: &Value, place: &Place) -> AirTraffic {
    let states = body["states"].as_array().cloned().unwrap_or_default();
    let mut airborne = 0;
    let mut on_ground = 0;
    let mut aircraft = vec![];
    for s in &states {
        let (Some(lon), Some(lat)) = (s[5].as_f64(), s[6].as_f64()) else {
            continue;
        };
        if haversine_km((place.lat, place.lon), (lat, lon)) > AIR_RADIUS_KM {
            continue;
        }
        if s[8].as_bool().unwrap_or(false) {
            on_ground += 1;
            continue;
        }
        airborne += 1;
        if aircraft.len() < 150 {
            aircraft.push(Aircraft {
                callsign: s[1].as_str().unwrap_or("").trim().to_string(),
                country: s[2].as_str().unwrap_or("").to_string(),
                lat,
                lon,
                altitude_m: s[13].as_f64().or_else(|| s[7].as_f64()),
                speed_kmh: s[9].as_f64().map(|v| v * 3.6),
                heading: s[10].as_f64(),
            });
        }
    }
    AirTraffic {
        place: place.id.clone(),
        name: place.name.clone(),
        airborne,
        on_ground,
        usual: None,
        z: None,
        aircraft,
        radius_km: AIR_RADIUS_KM,
    }
}

// ---------------------------------------------------------------- routes

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct RouteInfo {
    pub id: String,
    pub name: String,
    pub from: String,
    pub to: String,
    pub duration_s: f64,
    pub distance_m: f64,
    /// [lon, lat] pairs, simplified.
    pub geometry: Vec<[f64; 2]>,
    pub fetched_at: i64,
}

pub fn route_url(from: &Place, to: &Place) -> String {
    format!(
        "https://router.project-osrm.org/route/v1/driving/{:.5},{:.5};{:.5},{:.5}?overview=simplified&geometries=geojson",
        from.lon, from.lat, to.lon, to.lat
    )
}

pub fn parse_route(body: &Value) -> Option<(f64, f64, Vec<[f64; 2]>)> {
    if body["code"].as_str() != Some("Ok") {
        return None;
    }
    let r = &body["routes"][0];
    let geometry = r["geometry"]["coordinates"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|p| Some([p[0].as_f64()?, p[1].as_f64()?]))
                .take(400)
                .collect()
        })
        .unwrap_or_default();
    Some((r["duration"].as_f64()?, r["distance"].as_f64()?, geometry))
}

// ---------------------------------------------------------------- earthquakes

pub const QUAKES_URL: &str =
    "https://earthquake.usgs.gov/earthquakes/feed/v1.0/summary/2.5_day.geojson";
/// Quakes this strong are shown wherever they are; weaker ones only near a place.
pub const QUAKE_GLOBAL_MAG: f64 = 4.5;
pub const QUAKE_NEAR_KM: f64 = 500.0;

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Nearest {
    pub place: String,
    pub km: f64,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Quake {
    pub id: String,
    pub magnitude: f64,
    pub title: String,
    pub time: i64,
    pub url: String,
    pub lat: f64,
    pub lon: f64,
    pub depth_km: f64,
    pub tsunami: bool,
    /// USGS PAGER alert level (green/yellow/orange/red), when issued.
    pub alert: Option<String>,
    pub nearest: Option<Nearest>,
}

pub fn parse_quakes(body: &Value, places: &[Place]) -> Vec<Quake> {
    let mut out: Vec<Quake> = body["features"]
        .as_array()
        .map(|a| a.as_slice())
        .unwrap_or_default()
        .iter()
        .filter_map(|f| {
            let p = &f["properties"];
            let c = &f["geometry"]["coordinates"];
            let (lon, lat) = (c[0].as_f64()?, c[1].as_f64()?);
            let magnitude = p["mag"].as_f64()?;
            let nearest = places
                .iter()
                .map(|pl| Nearest {
                    place: pl.name.clone(),
                    km: haversine_km((pl.lat, pl.lon), (lat, lon)),
                })
                .min_by(|a, b| a.km.total_cmp(&b.km));
            let near = nearest.as_ref().is_some_and(|n| n.km <= QUAKE_NEAR_KM);
            if magnitude < QUAKE_GLOBAL_MAG && !near {
                return None;
            }
            let url = p["url"].as_str().unwrap_or("");
            Some(Quake {
                id: f["id"].as_str().unwrap_or("").to_string(),
                magnitude,
                title: p["title"]
                    .as_str()
                    .unwrap_or("")
                    .chars()
                    .take(200)
                    .collect(),
                time: p["time"].as_i64().unwrap_or(0) / 1000,
                url: if url.starts_with("https://earthquake.usgs.gov/") {
                    url.to_string()
                } else {
                    String::new()
                },
                lat,
                lon,
                depth_km: c[2].as_f64().unwrap_or(0.0),
                tsunami: p["tsunami"].as_i64() == Some(1),
                alert: p["alert"].as_str().map(str::to_string),
                nearest,
            })
        })
        .collect();
    out.sort_by_key(|q| std::cmp::Reverse(q.time));
    out.truncate(60);
    out
}

// ---------------------------------------------------------------- geocoding

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct PlaceCandidate {
    pub name: String,
    pub detail: String,
    pub lat: f64,
    pub lon: f64,
}

pub fn geocode_url(query: &str) -> String {
    let mut u = url::Url::parse("https://geocoding-api.open-meteo.com/v1/search").expect("static");
    u.query_pairs_mut()
        .append_pair("name", query)
        .append_pair("count", "8")
        .append_pair("language", "en");
    u.to_string()
}

pub fn parse_geocode(body: &Value) -> Vec<PlaceCandidate> {
    body["results"]
        .as_array()
        .map(|a| a.as_slice())
        .unwrap_or_default()
        .iter()
        .filter_map(|r| {
            let detail = [r["admin1"].as_str(), r["country"].as_str()]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(", ");
            Some(PlaceCandidate {
                name: r["name"].as_str()?.to_string(),
                detail,
                lat: r["latitude"].as_f64()?,
                lon: r["longitude"].as_f64()?,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn place(id: &str, lat: f64, lon: f64) -> Place {
        Place {
            id: id.into(),
            name: id.into(),
            lat,
            lon,
        }
    }

    #[test]
    fn haversine_dublin_london() {
        let km = haversine_km((53.3498, -6.2603), (51.5072, -0.1276));
        assert!((km - 463.0).abs() < 5.0, "{km}");
    }

    #[test]
    fn weather_baseline_and_outlook() {
        // 8 days of hourly data; "now" is the last day's hour 12.
        let n = 24 * 9;
        let times: Vec<String> = (0..n)
            .map(|i| format!("2026-11-{:02}T{:02}:00", 1 + i / 24, i % 24))
            .collect();
        let temps: Vec<f64> = (0..n)
            .map(|i| {
                if i % 24 == 12 {
                    10.0 + (i / 24 % 2) as f64 * 0.5
                } else {
                    5.0
                }
            })
            .collect();
        let mut probs = vec![0.0; n];
        probs[24 * 8 + 15] = 80.0;
        let body = json!({"latitude":1.0,"longitude":2.0,"timezone":"Europe/Dublin","utc_offset_seconds":0,
            "current":{"time":"2026-11-09T12:15","temperature_2m":16.0,"weather_code":61,"is_day":1,"wind_gusts_10m":5.0},
            "hourly":{"time":times,"temperature_2m":temps,"precipitation_probability":probs,"wind_gusts_10m":vec![5.0; n]}});
        let c = &parse_weather(&body, None, &[place("x", 1.0, 2.0)])[0];
        assert_eq!(c.summary, "Light rain");
        assert!(c.temp_vs_week.unwrap() > 5.0);
        assert!(c.temp_z.unwrap() > 4.0);
        assert_eq!(c.outlook.as_deref(), Some("Rain likely in 3h (80%)"));
        assert_eq!(c.next_hours.len(), 11);
    }

    #[test]
    fn parses_air_traffic_within_radius() {
        let p = place("x", 53.35, -6.26);
        let body = json!({"states":[
            ["a","EIN123 ","Ireland",0,0,-6.2,53.4,1000.0,false,200.0,90.0,0,null,1100.0],
            ["b","RYR1","Ireland",0,0,-6.25,53.42,null,true,0.0,0.0,0,null,null],
            ["c","FAR","X",0,0,-3.0,53.4,1000.0,false,200.0,90.0,0,null,null]
        ]});
        let a = parse_air(&body, &p);
        assert_eq!((a.airborne, a.on_ground), (1, 1));
        assert_eq!(a.aircraft[0].callsign, "EIN123");
        assert_eq!(a.aircraft[0].speed_kmh, Some(720.0));
    }

    #[test]
    fn filters_quakes_by_strength_or_proximity() {
        let body = json!({"features":[
            {"id":"far-small","properties":{"mag":3.0,"time":1000,"title":"t","url":"https://earthquake.usgs.gov/x"},"geometry":{"coordinates":[100.0,0.0,10.0]}},
            {"id":"far-big","properties":{"mag":6.1,"time":2000,"title":"t","url":"https://evil.example/x","tsunami":1},"geometry":{"coordinates":[100.0,0.0,10.0]}},
            {"id":"near-small","properties":{"mag":2.8,"time":3000,"title":"t","url":"https://earthquake.usgs.gov/y"},"geometry":{"coordinates":[-6.0,53.0,5.0]}}
        ]});
        let q = parse_quakes(&body, &[place("dublin", 53.35, -6.26)]);
        assert_eq!(
            q.iter().map(|q| q.id.as_str()).collect::<Vec<_>>(),
            vec!["near-small", "far-big"]
        );
        assert!(q[1].tsunami);
        assert_eq!(q[1].url, "");
    }

    #[test]
    fn parses_route_and_geocode() {
        let r = json!({"code":"Ok","routes":[{"duration":100.0,"distance":2000.0,"geometry":{"coordinates":[[1.0,2.0],[3.0,4.0]]}}]});
        assert_eq!(
            parse_route(&r),
            Some((100.0, 2000.0, vec![[1.0, 2.0], [3.0, 4.0]]))
        );
        assert_eq!(parse_route(&json!({"code":"NoRoute"})), None);
        let g = json!({"results":[{"name":"Cork","latitude":51.9,"longitude":-8.47,"admin1":"Munster","country":"Ireland"}]});
        assert_eq!(parse_geocode(&g)[0].detail, "Munster, Ireland");
    }
}
