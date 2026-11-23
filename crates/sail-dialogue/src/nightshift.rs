//! Night Shift (round 27): the owner's errands, shaped into work orders by the local model
//! during the day and solved overnight by Forge (`sail_forge::nightshift`).
//!
//! This module is the daemon's side of the feature:
//! - [`NightShift`] — the one service the HTTP routes, voice tools and Telegram share. It adds
//!   errands, runs the shaper in the background (one at a time, `Background` priority, retried
//!   every ten minutes while the model is unavailable), applies the owner's decisions and ships
//!   an approved result as a **draft** PR.
//! - [`MindShaper`] — `ShaperModel` over the local model.
//! - [`ForgeShipper`] — `Shipper` over `sail_forge::publish::ship_patch` (throwaway worktree on
//!   `origin/main`, draft PR, never merges, never touches the owner's checkout).
//! - [`SettingsExternalSolver`] — `ExternalSolver` over the owner's signed-in vendor CLI, chosen
//!   by Night Shift settings at the moment of the call.
//! - [`endpoint_picker`] — `EndpointPicker` over the Mesh pool, with the host as fallback.
//!
//! Nothing here composes owner-facing text with a model: work orders are labelled as SAIL's
//! draft by the clients, and morning text comes from `sail_forge::nightshift::morning`.

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use sail_forge::nightshift::{
    Endpoint, EndpointPicker, Errand, ErrandId, ErrandSource, ErrandState, ErrandStore,
    ExternalSolver, OwnerNote, RanOn, RanOnKind, ShaperModel, Shipper, Transition, WorkOrder,
    shape,
};
use sail_forge::publish::PublishConfig;
use sail_mind::escalation::provider::ExternalProvider;
use sail_protocol::{InferenceRequest, Message, Role};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

/// How often errands stuck in `Shaping` (model unavailable, daemon restarted) are retried.
/// Fixed a priori by the round-27 contract ("retried in 10 min").
const SHAPING_RETRY: Duration = Duration::from_secs(600);
/// Output budget for one shaper reply. Fixed a priori: a work order is a small JSON object
/// (title ≤ 100, why ≤ 600, ≤ 8 checks of ≤ 200, ≤ 6 paths, notes ≤ 1500 chars).
const SHAPER_MAX_TOKENS: u32 = 1_200;
/// Output budget handed to a vendor CLI for one external solve. The CLIs have no flag for it
/// today, so this only feeds SAIL's accounting (see `sail_mind::escalation::cli`).
const EXTERNAL_MAX_TOKENS: u32 = 16_000;

static GLOBAL: OnceLock<Arc<NightShift>> = OnceLock::new();

/// Install the daemon's Night Shift service. Returns false if one was already installed.
pub fn install(service: Arc<NightShift>) -> bool {
    GLOBAL.set(service).is_ok()
}

/// The daemon's Night Shift service, when the daemon started one.
pub fn global() -> Option<Arc<NightShift>> {
    GLOBAL.get().cloned()
}

/// Why an approval was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApproveError {
    /// The edited work order breaks the contract; plain-words reasons.
    Invalid(Vec<String>),
    /// Not approvable now (wrong state, unknown errand, storage error).
    Refused(String),
}

pub struct NightShift {
    store: Arc<ErrandStore>,
    shaper: Arc<dyn ShaperModel>,
    shipper: Arc<dyn Shipper>,
    repo_root: PathBuf,
    /// One shaper call at a time: the host model serves one background request well.
    shaping: tokio::sync::Mutex<()>,
}

impl NightShift {
    pub fn new(
        store: Arc<ErrandStore>,
        shaper: Arc<dyn ShaperModel>,
        shipper: Arc<dyn Shipper>,
        repo_root: PathBuf,
    ) -> Arc<Self> {
        Arc::new(Self {
            store,
            shaper,
            shipper,
            repo_root,
            shaping: tokio::sync::Mutex::new(()),
        })
    }

    pub fn store(&self) -> &Arc<ErrandStore> {
        &self.store
    }

    /// Start the background shaping retry loop (also shapes anything left in `Shaping` by a
    /// previous run). Call once, inside the Tokio runtime.
    pub fn start(self: &Arc<Self>) {
        let service = self.clone();
        tokio::spawn(async move {
            loop {
                let stuck: Vec<ErrandId> = service
                    .store
                    .list()
                    .into_iter()
                    .filter(|errand| matches!(errand.state, ErrandState::Shaping))
                    .map(|errand| errand.id)
                    .collect();
                for id in stuck {
                    service.shape_now(&id).await;
                }
                tokio::time::sleep(SHAPING_RETRY).await;
            }
        });
    }

    /// Record the owner's ask and start shaping it.
    pub fn add(self: &Arc<Self>, ask: &str, source: ErrandSource) -> Result<Errand, String> {
        let errand = self.store.add(ask.trim(), source, Utc::now(), random6())?;
        self.spawn_shaper(errand.id.clone());
        Ok(errand)
    }

    /// Send a proposed or unshapeable work order back to the shaper with the owner's note.
    pub fn reshape(self: &Arc<Self>, id: &ErrandId, note: &str) -> Result<Errand, String> {
        let errand = self
            .store
            .transition(id, Utc::now(), Transition::Reshape(note_now(note)))?;
        self.spawn_shaper(id.clone());
        Ok(errand)
    }

    /// Approve the proposed work order for tonight, optionally as edited by the owner. An
    /// edited order is validated against the repository exactly like a shaped one.
    pub fn approve(
        &self,
        id: &ErrandId,
        edited: Option<WorkOrder>,
    ) -> Result<Errand, ApproveError> {
        if let Some(order) = &edited {
            let root = self.repo_root.clone();
            let exists = move |path: &str| root.join(path).is_file();
            let reasons = shape::validate(order, &exists);
            if !reasons.is_empty() {
                return Err(ApproveError::Invalid(reasons));
            }
        }
        self.store
            .transition(id, Utc::now(), Transition::Approve(edited))
            .map_err(ApproveError::Refused)
    }

    /// Queue a finished errand again tonight with the owner's note (`## Owner feedback`).
    pub fn redo(&self, id: &ErrandId, note: &str) -> Result<Errand, String> {
        self.store
            .transition(id, Utc::now(), Transition::Redo(note_now(note)))
    }

    pub fn drop_errand(&self, id: &ErrandId) -> Result<Errand, String> {
        self.store.transition(id, Utc::now(), Transition::Drop)
    }

    /// The stored patch of a finished errand.
    pub fn diff(&self, id: &ErrandId) -> Option<String> {
        self.store.patch(id)
    }

    /// Ship a Ready result: `Shipping` now, then `Shipped` (draft PR URL) or back to `Ready`
    /// when pushing or opening the PR fails. Returns the `Shipping` errand.
    pub fn ship(self: &Arc<Self>, id: &ErrandId) -> Result<Errand, String> {
        let patch = self
            .store
            .patch(id)
            .filter(|patch| !patch.trim().is_empty())
            .ok_or_else(|| "there is no stored patch to ship".to_string())?;
        let shipping = self
            .store
            .transition(id, Utc::now(), Transition::Shipping)?;
        let ErrandState::Shipping { order, .. } = shipping.state.clone() else {
            return Err("the errand did not enter shipping".into());
        };
        let service = self.clone();
        let errand = shipping.clone();
        tokio::spawn(async move {
            let outcome = service.shipper.ship(&errand, &order, &patch).await;
            let transition = match outcome {
                Ok(url) => {
                    info!(errand = errand.id.as_str(), %url, "Night Shift shipped a draft PR");
                    Transition::Shipped(url)
                }
                Err(reason) => {
                    warn!(errand = errand.id.as_str(), %reason, "Night Shift ship failed");
                    Transition::ShipFailed(reason)
                }
            };
            // ShipFailed returns the reason as Err after moving the errand back to Ready.
            let _ = service.store.transition(&errand.id, Utc::now(), transition);
        });
        Ok(shipping)
    }

    fn spawn_shaper(self: &Arc<Self>, id: ErrandId) {
        let service = self.clone();
        tokio::spawn(async move { service.shape_now(&id).await });
    }

    async fn shape_now(&self, id: &ErrandId) {
        let _turn = self.shaping.lock().await;
        let still_shaping = self
            .store
            .get(id)
            .is_some_and(|errand| matches!(errand.state, ErrandState::Shaping));
        if !still_shaping {
            return;
        }
        if let Err(error) = shape::run_shaper(
            &self.store,
            self.shaper.as_ref(),
            &self.repo_root,
            id,
            Utc::now(),
            CancellationToken::new(),
        )
        .await
        {
            warn!(errand = id.as_str(), %error, "Night Shift shaping will be retried");
        }
    }
}

fn note_now(text: &str) -> OwnerNote {
    OwnerNote {
        at: Utc::now(),
        text: text.trim().to_string(),
    }
}

fn random6() -> [u8; 6] {
    let bytes = uuid::Uuid::new_v4().into_bytes();
    let mut out = [0u8; 6];
    out.copy_from_slice(&bytes[..6]);
    out
}

/// `ShaperModel` over the local model at `Background` priority, so live voice pre-empts it.
pub struct MindShaper {
    mind: sail_mind::MindService,
}

impl MindShaper {
    pub fn new(mind: sail_mind::MindService) -> Self {
        Self { mind }
    }
}

#[async_trait]
impl ShaperModel for MindShaper {
    async fn complete(
        &self,
        system: &str,
        user: &str,
        cancel: CancellationToken,
    ) -> Result<String, String> {
        let mut request = InferenceRequest::text("");
        request.messages = vec![
            Message::text(Role::System, system),
            Message::text(Role::User, user),
        ];
        request.generation.max_tokens = SHAPER_MAX_TOKENS;
        request.generation.temperature = 0.2;
        let response = self
            .mind
            .infer_with_priority(
                request,
                cancel,
                None,
                sail_protocol::resources::WorkPriority::Background,
            )
            .await
            .map_err(|error| error.to_string())?;
        Ok(response
            .message
            .content
            .text()
            .unwrap_or_default()
            .to_owned())
    }
}

/// `Shipper` over Forge's publish flow. The PR is a draft on `nightshift/<errand id>`.
pub struct ForgeShipper {
    config: PublishConfig,
}

impl ForgeShipper {
    pub fn new(config: PublishConfig) -> Self {
        Self { config }
    }
}

/// PR body for a shipped errand: the work order (with owner feedback), then the overnight
/// result in plain words. Pure.
pub fn pr_body(errand: &Errand, order: &WorkOrder) -> String {
    let mut body = order.to_issue_body(&errand.notes);
    if let ErrandState::Shipping { result, .. } | ErrandState::Ready { result, .. } = &errand.state
    {
        body.push_str("\n\n## Overnight result\n");
        body.push_str(&result.summary);
        body.push_str(&format!(
            "\n\nRan on {} ({}).",
            result.ran_on.name, result.ran_on.model
        ));
    }
    body.push_str(&format!(
        "\n\nGenerated overnight by SAIL Night Shift from the owner's request ({}), reviewed and shipped by the owner as a draft.",
        errand.id.as_str()
    ));
    body
}

#[async_trait]
impl Shipper for ForgeShipper {
    async fn ship(
        &self,
        errand: &Errand,
        order: &WorkOrder,
        patch: &str,
    ) -> Result<String, String> {
        let config = self.config.clone();
        let branch = format!("nightshift/{}", errand.id.as_str());
        let title = order.title.clone();
        let body = pr_body(errand, order);
        let patch = patch.to_string();
        tokio::task::spawn_blocking(move || {
            sail_forge::publish::ship_patch(&config, &branch, &title, &body, &patch)
        })
        .await
        .map_err(|error| format!("the ship task stopped: {error}"))?
    }
}

/// `ExternalSolver` over the owner's signed-in vendor CLI. The CLI is chosen from Night Shift
/// settings when the call happens; a provider configured in the escalation policy file with
/// the same adapter supplies its model/command, otherwise the CLI's defaults are used.
pub struct SettingsExternalSolver {
    store: Arc<ErrandStore>,
}

impl SettingsExternalSolver {
    pub fn new(store: Arc<ErrandStore>) -> Self {
        Self { store }
    }

    fn adapter(&self) -> Option<String> {
        self.store
            .settings()
            .external_provider
            .filter(|adapter| matches!(adapter.as_str(), "claude_cli" | "codex_cli"))
    }
}

fn vendor_label(adapter: &str) -> &'static str {
    match adapter {
        "claude_cli" => "Claude Code",
        "codex_cli" => "Codex",
        _ => "external CLI",
    }
}

fn provider_config(adapter: &str) -> sail_mind::escalation::ProviderConfig {
    let policy_path = sail_mind::escalation::service::default_policy_path();
    let configured = sail_mind::escalation::service::load_policy(&policy_path)
        .ok()
        .and_then(|policy| {
            policy
                .providers
                .into_iter()
                .find(|provider| provider.adapter == adapter)
        });
    configured.unwrap_or_else(|| sail_mind::escalation::ProviderConfig {
        id: format!("nightshift-{adapter}"),
        adapter: adapter.to_string(),
        base_url: String::new(),
        model: String::new(),
        api_key_env: String::new(),
        command: None,
        cli_unverified_ok: false,
        kinds: Vec::new(),
        input_price_micros_per_mtok: 0,
        output_price_micros_per_mtok: 0,
        enabled: true,
    })
}

#[async_trait]
impl ExternalSolver for SettingsExternalSolver {
    fn ran_on(&self) -> RanOn {
        let adapter = self.adapter().unwrap_or_default();
        let model = if adapter.is_empty() {
            String::new()
        } else {
            provider_config(&adapter).model
        };
        RanOn {
            kind: RanOnKind::External,
            name: vendor_label(&adapter).into(),
            model: if model.is_empty() {
                "CLI default".into()
            } else {
                model
            },
        }
    }

    async fn solve(
        &self,
        system: &str,
        prompt: &str,
        cancel: CancellationToken,
    ) -> Result<String, String> {
        let adapter = self
            .adapter()
            .ok_or_else(|| "no external CLI is chosen in Night Shift settings".to_string())?;
        let provider = sail_mind::escalation::cli::CliProvider::new(provider_config(&adapter))
            .map_err(|error| format!("{error:?}"))?;
        let reply = provider
            .complete(system, prompt, EXTERNAL_MAX_TOKENS, cancel)
            .await
            .map_err(|error| format!("{error:?}"))?;
        Ok(reply.text)
    }
}

/// `EndpointPicker` over the Mesh pool: a Ready, non-resting worker when allowed, else the
/// host (Forge's own endpoint probing, signalled by an empty URL and no client).
pub fn endpoint_picker(mind: sail_mind::MindService) -> Arc<dyn EndpointPicker> {
    if let Some(service) = global() {
        watch_pooled_errands(service.store(), mind.clone());
    }
    Arc::new(MeshPicker { mind })
}

fn watch_pooled_errands(store: &Arc<ErrandStore>, mind: sail_mind::MindService) {
    use tokio::sync::broadcast::error::RecvError;

    let mut updates = store.subscribe();
    let store = Arc::downgrade(store);
    tokio::spawn(async move {
        loop {
            if let Some(lane) = mind.heavy() {
                let Some(store) = store.upgrade() else { return };
                let job = store
                    .list()
                    .into_iter()
                    .filter_map(|errand| match errand.state {
                        ErrandState::Working {
                            order,
                            started_at,
                            ran_on,
                            ..
                        } if ran_on.kind == RanOnKind::Pooled => Some((started_at, order.title)),
                        _ => None,
                    })
                    .max_by_key(|(started_at, _)| *started_at);
                lane.set_job(
                    job.map(|(started_at, title)| sail_protocol::mesh::HeavyJob {
                        title,
                        since: started_at.to_rfc3339(),
                    }),
                );
            }
            match updates.recv().await {
                Ok(_) | Err(RecvError::Lagged(_)) => {}
                Err(RecvError::Closed) => return,
            }
        }
    });
}

struct MeshPicker {
    mind: sail_mind::MindService,
}

impl EndpointPicker for MeshPicker {
    fn heavy(&self) -> Option<Endpoint> {
        let endpoint = self.mind.heavy()?.endpoint()?;
        static CLIENT: std::sync::OnceLock<Option<reqwest::Client>> = std::sync::OnceLock::new();
        let client = CLIENT
            .get_or_init(|| {
                reqwest::Client::builder()
                    .no_proxy()
                    .redirect(reqwest::redirect::Policy::none())
                    .build()
                    .ok()
            })
            .as_ref()?
            .clone();
        Some(Endpoint {
            url: endpoint.url,
            ran_on: RanOn {
                kind: RanOnKind::Pooled,
                name: endpoint.worker,
                model: endpoint.model,
            },
            client: Some(client),
        })
    }

    fn pooled_timing(&self) -> Option<sail_forge::nightshift::CompletionTiming> {
        self.mind.heavy()?.completion_timing_observer()
    }

    fn pick(&self, use_mesh: bool) -> Endpoint {
        if use_mesh && let Some(worker) = self.mind.mesh().and_then(|pool| pool.night_worker()) {
            return Endpoint {
                url: worker.url,
                ran_on: RanOn {
                    kind: RanOnKind::Mesh,
                    name: worker.name,
                    model: worker.model,
                },
                client: Some(worker.client),
            };
        }
        self.host()
    }

    fn host(&self) -> Endpoint {
        Endpoint {
            url: String::new(),
            ran_on: RanOn {
                kind: RanOnKind::Host,
                name: "host".into(),
                model: sail_common::models::primary_model_path()
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "local".into()),
            },
            client: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vendor_labels_are_plain_words() {
        assert_eq!(vendor_label("claude_cli"), "Claude Code");
        assert_eq!(vendor_label("codex_cli"), "Codex");
        assert_eq!(vendor_label("other"), "external CLI");
    }

    #[test]
    fn random_ids_are_valid_errand_ids() {
        for _ in 0..50 {
            let id = ErrandId::new(random6());
            assert!(ErrandId::parse(id.as_str()).is_some(), "{}", id.as_str());
        }
    }
}
