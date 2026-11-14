//! The market service (round 20, Claude): catalog cache and refresh, search, stage,
//! install, uninstall. Owner-only callers (`/api/hermes/market`, `sail-hermes market`).
use super::{
    CATALOG_TTL_SECS, Catalog, Installed, Kind, Listing, LockEntry, PLUGIN_CATALOG_URL, Page,
    Query, SKILLS_INDEX_URL, StagedReport, catalog,
    fetch::{self, Http},
    install::Store,
    now_secs,
};
use serde::Serialize;
use std::{
    path::PathBuf,
    sync::{Arc, Mutex, OnceLock},
};

const SKILLS_INDEX_LIMIT: u64 = 160 * 1024 * 1024;
const PLUGIN_CATALOG_LIMIT: u64 = 48 * 1024 * 1024;
/// Quarantined items nobody installed are removed after a day.
const STAGE_MAX_AGE_SECS: i64 = 24 * 3600;

#[derive(Debug, Clone)]
pub struct MarketPaths {
    /// `$SAIL_DATA/market`
    pub market: PathBuf,
    /// `$SAIL_DATA/skills`
    pub skills: PathBuf,
    /// `$SAIL_DATA/plugins/hermes`
    pub plugins: PathBuf,
}

impl MarketPaths {
    pub fn default_paths() -> Self {
        Self {
            market: sail_common::storage::data_path("market"),
            skills: sail_common::storage::data_path("skills"),
            plugins: sail_common::storage::data_path("plugins").join("hermes"),
        }
    }

    pub fn store(&self) -> Store {
        Store {
            market: self.market.clone(),
            skills: self.skills.clone(),
            plugins: self.plugins.clone(),
        }
    }

    fn cache(&self) -> PathBuf {
        self.market.join("catalog.json")
    }
}

/// What the market page shows about the catalogs.
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct CatalogStatus {
    pub skills: usize,
    pub plugins: usize,
    pub skills_generated_at: String,
    pub plugins_generated_at: String,
    pub skills_fetched_at: i64,
    pub plugins_fetched_at: i64,
    pub skipped: usize,
    pub refreshing: bool,
    /// The last refresh error; the cached catalog stays usable.
    pub error: Option<String>,
    /// True when either catalog is older than the TTL (or was never fetched).
    pub stale: bool,
}

pub struct Market {
    pub paths: MarketPaths,
    http: Arc<dyn Http>,
    hermes: Option<crate::hermes::Paths>,
    catalog: Mutex<Option<Arc<Catalog>>>,
    state: Mutex<(bool, Option<String>, usize)>, // refreshing, last error, skipped
    refresh_gate: tokio::sync::Mutex<()>,
}

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| format!("worker failed: {e}"))?
}

impl Market {
    pub fn new(
        paths: MarketPaths,
        http: Arc<dyn Http>,
        hermes: Option<crate::hermes::Paths>,
    ) -> Self {
        Self {
            paths,
            http,
            hermes,
            catalog: Mutex::new(None),
            state: Mutex::new((false, None, 0)),
            refresh_gate: tokio::sync::Mutex::new(()),
        }
    }

    /// The cached catalog, loaded from disk on first use (empty if there is none yet).
    pub async fn catalog(&self) -> Arc<Catalog> {
        if let Some(c) = self.catalog.lock().unwrap().clone() {
            return c;
        }
        let path = self.paths.cache();
        let loaded = blocking(move || {
            Ok(std::fs::read(&path)
                .ok()
                .and_then(|b| serde_json::from_slice::<Catalog>(&b).ok())
                .unwrap_or_default())
        })
        .await
        .unwrap_or_default();
        let mut slot = self.catalog.lock().unwrap();
        slot.get_or_insert_with(|| Arc::new(loaded)).clone()
    }

    pub async fn status(&self) -> CatalogStatus {
        let c = self.catalog().await;
        let (refreshing, error, skipped) = self.state.lock().unwrap().clone();
        let now = now_secs();
        CatalogStatus {
            skills: c.skills.len(),
            plugins: c.plugins.len(),
            skills_generated_at: c.skills_generated_at.clone(),
            plugins_generated_at: c.plugins_generated_at.clone(),
            skills_fetched_at: c.skills_fetched_at,
            plugins_fetched_at: c.plugins_fetched_at,
            skipped,
            refreshing,
            error,
            stale: now - c.skills_fetched_at > CATALOG_TTL_SECS
                || now - c.plugins_fetched_at > CATALOG_TTL_SECS,
        }
    }

    /// Start a refresh in the background when the catalog is stale and none is running.
    pub fn refresh_if_stale(self: &Arc<Self>) {
        let me = self.clone();
        tokio::spawn(async move {
            if me.status().await.stale && !me.state.lock().unwrap().0 {
                let _ = me.refresh(false).await;
            }
        });
    }

    /// Download both catalogs (only stale ones unless `force`). Each catalog is replaced
    /// only when it parsed; otherwise the cached one stays and the error is reported.
    pub async fn refresh(&self, force: bool) -> Result<CatalogStatus, String> {
        let _gate = self.refresh_gate.lock().await;
        self.state.lock().unwrap().0 = true;
        let result = self.refresh_inner(force).await;
        {
            let mut state = self.state.lock().unwrap();
            state.0 = false;
            state.1 = result.as_ref().err().cloned();
        }
        result?;
        Ok(self.status().await)
    }

    async fn refresh_inner(&self, force: bool) -> Result<(), String> {
        let current = self.catalog().await;
        let now = now_secs();
        let mut next = (*current).clone();
        let mut errors = Vec::new();
        let mut skipped = 0;
        if force || now - current.skills_fetched_at > CATALOG_TTL_SECS {
            match self.http.get(SKILLS_INDEX_URL, SKILLS_INDEX_LIMIT).await {
                Ok(bytes) => match blocking(move || catalog::parse_skills_index(&bytes)).await {
                    Ok((generated, skills, stats)) => {
                        next.skills = skills;
                        next.skills_generated_at = generated;
                        next.skills_fetched_at = now;
                        skipped += stats.skipped;
                    }
                    Err(e) => errors.push(format!("skills index: {e}")),
                },
                Err(e) => errors.push(format!("skills index: {e}")),
            }
        }
        if force || now - current.plugins_fetched_at > CATALOG_TTL_SECS {
            match self
                .http
                .get(PLUGIN_CATALOG_URL, PLUGIN_CATALOG_LIMIT)
                .await
            {
                Ok(bytes) => match blocking(move || catalog::parse_plugin_catalog(&bytes)).await {
                    Ok((generated, plugins, removed, stats)) => {
                        next.plugins = plugins;
                        next.removed = removed;
                        next.plugins_generated_at = generated;
                        next.plugins_fetched_at = now;
                        skipped += stats.skipped;
                    }
                    Err(e) => errors.push(format!("plugin catalog: {e}")),
                },
                Err(e) => errors.push(format!("plugin catalog: {e}")),
            }
        }
        if next != *current {
            let next = Arc::new(next);
            let (path, saved) = (self.paths.cache(), next.clone());
            blocking(move || {
                if let Some(dir) = path.parent() {
                    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
                }
                let tmp = path.with_extension("json.tmp");
                std::fs::write(
                    &tmp,
                    serde_json::to_vec(&*saved).map_err(|e| e.to_string())?,
                )
                .map_err(|e| e.to_string())?;
                std::fs::rename(&tmp, &path).map_err(|e| e.to_string())
            })
            .await?;
            *self.catalog.lock().unwrap() = Some(next);
        }
        self.state.lock().unwrap().2 = skipped;
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join("; "))
        }
    }

    pub async fn search(&self, query: Query) -> Result<Page, String> {
        let catalog = self.catalog().await;
        let store = self.paths.store();
        blocking(move || {
            let lock = store.load_lock()?;
            Ok(catalog::search(&catalog, &query, &lock))
        })
        .await
    }

    pub async fn item(&self, kind: Kind, id: &str) -> Result<Listing, String> {
        catalog::find(&*self.catalog().await, kind, id)
            .ok_or_else(|| format!("no {} {id:?} in the market", kind_name(kind)))
    }

    /// Download one item into quarantine and scan it. Nothing is installed.
    pub async fn stage(&self, kind: Kind, id: &str) -> Result<StagedReport, String> {
        let listing = self.item(kind, id).await?;
        let bundle = fetch::fetch(self.http.clone(), &listing.route).await?;
        let store = self.paths.store();
        let stage_id = new_stage_id();
        blocking(move || {
            let _ = store.sweep(now_secs(), STAGE_MAX_AGE_SECS);
            store.stage(&stage_id, listing, &bundle, now_secs())
        })
        .await
    }

    pub async fn install(&self, stage_id: &str, acknowledge: bool) -> Result<LockEntry, String> {
        let (store, stage_id) = (self.paths.store(), stage_id.to_string());
        blocking(move || store.install(&stage_id, acknowledge, now_secs())).await
    }

    pub async fn discard(&self, stage_id: &str) -> Result<(), String> {
        let (store, stage_id) = (self.paths.store(), stage_id.to_string());
        blocking(move || store.discard(&stage_id)).await
    }

    pub async fn installed(&self) -> Result<Vec<Installed>, String> {
        let catalog = self.catalog().await;
        let store = self.paths.store();
        let mut list = blocking(move || store.installed()).await?;
        for item in &mut list {
            if item.entry.kind == Kind::Plugin {
                item.removed = catalog
                    .removed
                    .iter()
                    .find(|r| r.name == item.entry.id)
                    .cloned();
            }
        }
        Ok(list)
    }

    pub async fn uninstall(&self, key: &str, force: bool) -> Result<(), String> {
        let (store, key, hermes) = (self.paths.store(), key.to_string(), self.hermes.clone());
        blocking(move || store.uninstall(&key, force, hermes.as_ref())).await
    }
}

fn kind_name(kind: Kind) -> &'static str {
    match kind {
        Kind::Skill => "skill",
        Kind::Plugin => "plugin",
    }
}

/// 32 lowercase hex characters from the OS random source.
fn new_stage_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

/// The daemon's market (default paths, real network).
pub fn global() -> Arc<Market> {
    static MARKET: OnceLock<Arc<Market>> = OnceLock::new();
    MARKET
        .get_or_init(|| {
            let http: Arc<dyn Http> = match fetch::ReqwestHttp::new() {
                Ok(h) => Arc::new(h),
                Err(_) => Arc::new(fetch::FakeHttp::default()),
            };
            Arc::new(Market::new(
                MarketPaths::default_paths(),
                http,
                Some(crate::hermes::Paths::default_paths()),
            ))
        })
        .clone()
}
