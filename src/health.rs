use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Shared health state for the bot.
/// Updated by PolyWs and WeatherStrategy, read by the heartbeat in the main loop.
pub struct BotHealth {
    inner: Mutex<Inner>,
}

struct Inner {
    started_at: Instant,
    // WS
    ws_reconnects: u32,
    ws_events_total: u64,
    ws_connected_at: Option<Instant>,
    ws_last_event_at: Option<Instant>,
    // Weather
    weather_last_scan_at: Option<Instant>,
    weather_edges_today: u32,
    weather_scans_today: u32,
    weather_date: Option<chrono::NaiveDate>,
}

pub struct HealthSummary {
    pub uptime: Duration,
    pub ws_reconnects: u32,
    pub ws_events_total: u64,
    pub ws_age: Option<Duration>,
    pub ws_last_event_ago: Option<Duration>,
    pub weather_last_scan_ago: Option<Duration>,
    pub weather_edges_today: u32,
    pub weather_scans_today: u32,
}

impl BotHealth {
    pub fn new() -> Self {
        BotHealth {
            inner: Mutex::new(Inner {
                started_at: Instant::now(),
                ws_reconnects: 0,
                ws_events_total: 0,
                ws_connected_at: None,
                ws_last_event_at: None,
                weather_last_scan_at: None,
                weather_edges_today: 0,
                weather_scans_today: 0,
                weather_date: None,
            }),
        }
    }

    /// First WS connection established.
    pub fn ws_connected(&self) {
        if let Ok(mut h) = self.inner.lock() {
            h.ws_connected_at = Some(Instant::now());
        }
    }

    /// WS reconnected (increments counter + resets connection age).
    pub fn ws_reconnected(&self) {
        if let Ok(mut h) = self.inner.lock() {
            h.ws_reconnects += 1;
            h.ws_connected_at = Some(Instant::now());
        }
    }

    /// Batch update: called once per minute with that minute's event count.
    pub fn ws_events(&self, count: u64) {
        if let Ok(mut h) = self.inner.lock() {
            h.ws_events_total += count;
            h.ws_last_event_at = Some(Instant::now());
        }
    }

    /// Weather scan completed. Resets daily counters on date change.
    pub fn weather_scan_complete(&self, edges: u32) {
        if let Ok(mut h) = self.inner.lock() {
            let today = chrono::Utc::now().date_naive();
            if h.weather_date != Some(today) {
                h.weather_edges_today = 0;
                h.weather_scans_today = 0;
                h.weather_date = Some(today);
            }
            h.weather_last_scan_at = Some(Instant::now());
            h.weather_edges_today += edges;
            h.weather_scans_today += 1;
        }
    }

    pub fn summary(&self) -> HealthSummary {
        let h = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        HealthSummary {
            uptime: h.started_at.elapsed(),
            ws_reconnects: h.ws_reconnects,
            ws_events_total: h.ws_events_total,
            ws_age: h.ws_connected_at.map(|t| t.elapsed()),
            ws_last_event_ago: h.ws_last_event_at.map(|t| t.elapsed()),
            weather_last_scan_ago: h.weather_last_scan_at.map(|t| t.elapsed()),
            weather_edges_today: h.weather_edges_today,
            weather_scans_today: h.weather_scans_today,
        }
    }
}

pub fn format_duration(d: Duration) -> String {
    let secs = d.as_secs();
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else {
        let h = secs / 3600;
        let m = (secs % 3600) / 60;
        format!("{h}h {m}m")
    }
}
