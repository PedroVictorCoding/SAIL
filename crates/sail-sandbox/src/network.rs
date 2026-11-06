//! Default-deny destination policy and the authorization core of `SailNetworkBroker`.
//!
//! Nothing here opens a connection by itself. The proxy resolves a destination once, validates
//! every resolved address against blocked ranges, and must connect to one of the returned
//! addresses so a second DNS answer cannot redirect the connection (DNS rebinding).
use async_trait::async_trait;
use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use tokio::sync::watch;
use uuid::Uuid;

use sail_common::types::SubsystemId;

pub const MAX_POLICY_RULES: usize = 128;
pub const MAX_POLICY_PORTS: usize = 8;
pub const MAX_POLICY_ID: usize = 128;
pub const MAX_GRANT_TTL_MS: i64 = 60 * 60 * 1_000;
pub const MAX_ACTIVE_CONNECTIONS: u32 = 256;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NetworkError {
    #[error("network policy is invalid: {0}")]
    InvalidPolicy(String),
    #[error("destination host is invalid: {0}")]
    InvalidHost(String),
    #[error("destination is denied: {0}")]
    Denied(String),
    #[error("destination resolution failed: {0}")]
    Resolve(String),
    #[error("network grant was not found")]
    GrantNotFound,
    #[error("network grant has expired")]
    GrantExpired,
    #[error("network grant has been revoked")]
    GrantRevoked,
    #[error("network policy or version is unavailable")]
    PolicyUnavailable,
    #[error("network policy version is stale or conflicting")]
    PolicyVersion,
    #[error("network I/O failed: {0}")]
    Io(String),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NetworkPolicyRef {
    pub policy_id: String,
    pub policy_version: u64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NetworkLimits {
    pub max_connections: u32,
    pub max_bytes_per_connection: u64,
    pub max_bps: u64,
    pub connect_timeout_ms: u64,
    pub idle_timeout_ms: u64,
    pub max_dns_queries: u32,
}

impl Default for NetworkLimits {
    fn default() -> Self {
        Self {
            max_connections: 8,
            max_bytes_per_connection: 64 * 1024 * 1024,
            max_bps: 16 * 1024 * 1024,
            connect_timeout_ms: 10_000,
            idle_timeout_ms: 30_000,
            max_dns_queries: 8,
        }
    }
}

impl NetworkLimits {
    pub fn validate(&self) -> Result<(), NetworkError> {
        if self.max_connections == 0
            || self.max_connections > MAX_ACTIVE_CONNECTIONS
            || self.max_bytes_per_connection == 0
            || self.max_bytes_per_connection > 1024 * 1024 * 1024
            || self.max_bps == 0
            || self.max_bps > 1024 * 1024 * 1024
            || !(1_000..=120_000).contains(&self.connect_timeout_ms)
            || !(1_000..=300_000).contains(&self.idle_timeout_ms)
            || self.max_dns_queries == 0
            || self.max_dns_queries > 64
        {
            return Err(NetworkError::InvalidPolicy("network limits".into()));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Effect {
    Allow,
    Deny,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProxyProtocol {
    Http,
    Https,
    Socks5,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum HostPattern {
    Exact(String),
    /// Matches strict subdomains only: `*.example.com` matches `api.example.com` but not
    /// `example.com` itself. This prevents an apex typo from widening a grant.
    Subdomain(String),
    Any,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NetworkRule {
    pub effect: Effect,
    pub host: HostPattern,
    #[serde(default)]
    pub ports: Vec<u16>,
    #[serde(default)]
    pub protocols: Vec<ProxyProtocol>,
    #[serde(default)]
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NetworkPolicy {
    pub id: String,
    pub version: u64,
    pub rules: Vec<NetworkRule>,
    pub limits: NetworkLimits,
    #[cfg(test)]
    #[serde(skip)]
    pub(crate) allow_loopback: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleMatch {
    pub effect: Effect,
    pub rule_index: usize,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkViolation {
    pub policy_id: String,
    pub policy_version: u64,
    pub rule: String,
    pub operation: String,
    pub destination: String,
    pub reason: String,
}

impl fmt::Display for NetworkViolation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "network {} to {} denied by {} ({}): {}",
            self.operation, self.destination, self.policy_id, self.rule, self.reason
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AllowedDestination {
    pub host: String,
    pub port: u16,
    pub protocol: ProxyProtocol,
    pub policy_id: String,
    pub policy_version: u64,
    pub matched_rule: String,
    /// The exact addresses validated now. The proxy must connect to one of these and must not
    /// resolve the hostname again.
    pub ips: Vec<IpAddr>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NetworkDecision {
    Allowed(AllowedDestination),
    Denied(NetworkViolation),
}

#[async_trait]
pub trait Resolver: Send + Sync {
    async fn resolve(&self, host: &str, port: u16) -> Result<Vec<IpAddr>, NetworkError>;
}

pub struct SystemResolver;

#[async_trait]
impl Resolver for SystemResolver {
    async fn resolve(&self, host: &str, port: u16) -> Result<Vec<IpAddr>, NetworkError> {
        let addrs = tokio::net::lookup_host((host, port))
            .await
            .map_err(|error| NetworkError::Resolve(error.to_string()))?;
        let mut ips: Vec<IpAddr> = addrs.map(|addr| addr.ip()).collect();
        ips.sort();
        ips.dedup();
        Ok(ips)
    }
}

impl NetworkPolicy {
    pub fn new(
        id: impl Into<String>,
        version: u64,
        rules: Vec<NetworkRule>,
        limits: NetworkLimits,
    ) -> Self {
        Self {
            id: id.into(),
            version,
            rules,
            limits,
            #[cfg(test)]
            allow_loopback: false,
        }
    }

    pub fn validate(&self) -> Result<(), NetworkError> {
        validate_policy_id(&self.id)?;
        if self.version == 0 || self.rules.len() > MAX_POLICY_RULES {
            return Err(NetworkError::InvalidPolicy("policy header".into()));
        }
        self.limits.validate()?;
        for rule in &self.rules {
            validate_rule(rule)?;
        }
        Ok(())
    }

    /// Deny precedence: every deny rule is checked before any allow rule. No match means deny.
    #[allow(clippy::result_large_err)] // The denial carries the policy and destination for audit.
    pub fn evaluate(
        &self,
        host: &str,
        port: u16,
        protocol: ProxyProtocol,
    ) -> Result<RuleMatch, NetworkDecision> {
        if let Some((index, rule)) = self
            .rules
            .iter()
            .enumerate()
            .find(|(_, rule)| rule.effect == Effect::Deny && rule.matches(host, port, protocol))
        {
            return Err(NetworkDecision::Denied(self.violation(
                index,
                "connect",
                host,
                port,
                &rule.reason_or("deny rule"),
            )));
        }
        if let Some((index, rule)) =
            self.rules.iter().enumerate().find(|(_, rule)| {
                rule.effect == Effect::Allow && rule.matches(host, port, protocol)
            })
        {
            return Ok(RuleMatch {
                effect: Effect::Allow,
                rule_index: index,
                reason: rule.reason_or("allow rule"),
            });
        }
        Err(NetworkDecision::Denied(self.violation(
            0,
            "connect",
            host,
            port,
            "default deny",
        )))
    }

    fn violation(
        &self,
        rule_index: usize,
        operation: &str,
        host: &str,
        port: u16,
        reason: &str,
    ) -> NetworkViolation {
        NetworkViolation {
            policy_id: self.id.clone(),
            policy_version: self.version,
            rule: format!("rule[{rule_index}]"),
            operation: operation.into(),
            destination: format!("{host}:{port}"),
            reason: reason.into(),
        }
    }

    fn address_blocked(&self, ip: &IpAddr) -> bool {
        #[cfg(test)]
        if self.allow_loopback && ip.is_loopback() {
            return false;
        }
        is_blocked_ip(ip)
    }
}

impl NetworkRule {
    fn matches(&self, host: &str, port: u16, protocol: ProxyProtocol) -> bool {
        if !self.ports.is_empty() && !self.ports.contains(&port) {
            return false;
        }
        if !self.protocols.is_empty() && !self.protocols.contains(&protocol) {
            return false;
        }
        self.host.matches(host)
    }

    fn reason_or(&self, fallback: &str) -> String {
        if self.reason.is_empty() {
            fallback.into()
        } else {
            self.reason.clone()
        }
    }
}

impl HostPattern {
    pub fn matches(&self, host: &str) -> bool {
        match self {
            HostPattern::Exact(expected) => normalized_pattern(expected) == host,
            HostPattern::Subdomain(suffix) => {
                let suffix = normalized_pattern(suffix);
                host.len() > suffix.len() + 1
                    && host.ends_with(&suffix)
                    && host.as_bytes()[host.len() - suffix.len() - 1] == b'.'
            }
            HostPattern::Any => true,
        }
    }
}

fn normalized_pattern(value: &str) -> String {
    value.trim().trim_end_matches('.').to_ascii_lowercase()
}

/// Resolve once, then validate every answer. A resolver that returns a public and a private
/// address is denied, not partially trusted.
pub async fn authorize_connect(
    policy: &NetworkPolicy,
    host: &str,
    port: u16,
    protocol: ProxyProtocol,
    resolver: &dyn Resolver,
) -> NetworkDecision {
    let normalized = match normalize_host(host) {
        Ok(host) => host,
        Err(error) => {
            return NetworkDecision::Denied(NetworkViolation {
                policy_id: policy.id.clone(),
                policy_version: policy.version,
                rule: "interface".into(),
                operation: "connect".into(),
                destination: format!("{host}:{port}"),
                reason: error.to_string(),
            });
        }
    };
    if port == 0 {
        return NetworkDecision::Denied(NetworkViolation {
            policy_id: policy.id.clone(),
            policy_version: policy.version,
            rule: "interface".into(),
            operation: "connect".into(),
            destination: format!("{host}:{port}"),
            reason: "port is zero".into(),
        });
    }
    let matched = match policy.evaluate(&normalized.host, port, protocol) {
        Ok(matched) => matched,
        Err(denied) => return denied,
    };
    let ips = match normalized.literal {
        Some(ip) => vec![ip],
        None => match resolver.resolve(&normalized.host, port).await {
            Ok(ips) => ips,
            Err(error) => {
                return NetworkDecision::Denied(NetworkViolation {
                    policy_id: policy.id.clone(),
                    policy_version: policy.version,
                    rule: format!("rule[{}]", matched.rule_index),
                    operation: "resolve".into(),
                    destination: format!("{}:{port}", normalized.host),
                    reason: error.to_string(),
                });
            }
        },
    };
    if ips.is_empty() {
        return NetworkDecision::Denied(NetworkViolation {
            policy_id: policy.id.clone(),
            policy_version: policy.version,
            rule: format!("rule[{}]", matched.rule_index),
            operation: "resolve".into(),
            destination: format!("{}:{port}", normalized.host),
            reason: "no address".into(),
        });
    }
    if let Some(blocked) = ips.iter().find(|ip| policy.address_blocked(ip)) {
        return NetworkDecision::Denied(NetworkViolation {
            policy_id: policy.id.clone(),
            policy_version: policy.version,
            rule: format!("rule[{}]", matched.rule_index),
            operation: "connect".into(),
            destination: format!("{}:{port}", normalized.host),
            reason: format!("resolved address {blocked} is in a prohibited range"),
        });
    }
    NetworkDecision::Allowed(AllowedDestination {
        host: normalized.host,
        port,
        protocol,
        policy_id: policy.id.clone(),
        policy_version: policy.version,
        matched_rule: format!("rule[{}]: {}", matched.rule_index, matched.reason),
        ips,
    })
}

pub fn validate_policy_id(id: &str) -> Result<(), NetworkError> {
    if id.is_empty()
        || id.len() > MAX_POLICY_ID
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(NetworkError::InvalidPolicy("policy id".into()));
    }
    Ok(())
}

fn validate_rule(rule: &NetworkRule) -> Result<(), NetworkError> {
    if rule.ports.len() > MAX_POLICY_PORTS || rule.ports.contains(&0) {
        return Err(NetworkError::InvalidPolicy("rule ports".into()));
    }
    if rule.protocols.len() > 3 || (rule.effect == Effect::Allow && rule.protocols.is_empty()) {
        return Err(NetworkError::InvalidPolicy("rule protocols".into()));
    }
    if rule.effect == Effect::Allow && rule.ports.is_empty() {
        return Err(NetworkError::InvalidPolicy(
            "allow rules must state ports".into(),
        ));
    }
    if rule.effect == Effect::Allow && rule.host == HostPattern::Any {
        return Err(NetworkError::InvalidPolicy(
            "allow rules cannot use an any-host pattern".into(),
        ));
    }
    match &rule.host {
        HostPattern::Exact(host) => {
            normalize_host(host)?;
        }
        HostPattern::Subdomain(host) => {
            let normalized = normalize_host(host)?;
            if !normalized.host.contains('.') {
                return Err(NetworkError::InvalidPolicy(
                    "subdomain pattern needs at least two labels".into(),
                ));
            }
        }
        HostPattern::Any => {}
    }
    if rule.reason.len() > 200 || rule.reason.chars().any(char::is_control) {
        return Err(NetworkError::InvalidPolicy("rule reason".into()));
    }
    Ok(())
}

#[derive(Debug, Clone)]
struct NormalizedHost {
    host: String,
    literal: Option<IpAddr>,
}

fn normalize_host(host: &str) -> Result<NormalizedHost, NetworkError> {
    let host = host.trim();
    if host.is_empty() || host.len() > 253 || !host.is_ascii() {
        return Err(NetworkError::InvalidHost(host.into()));
    }
    if host.starts_with('[') && host.ends_with(']') {
        let inner = &host[1..host.len() - 1];
        let ip: Ipv6Addr = inner
            .parse()
            .map_err(|_| NetworkError::InvalidHost(host.into()))?;
        return Ok(NormalizedHost {
            host: ip.to_string(),
            literal: Some(IpAddr::V6(ip)),
        });
    }
    if let Ok(ip) = host.parse::<IpAddr>() {
        return Ok(NormalizedHost {
            host: ip.to_string(),
            literal: Some(ip),
        });
    }
    let host = host.strip_suffix('.').unwrap_or(host).to_ascii_lowercase();
    if host.is_empty()
        || host.starts_with('.')
        || host.ends_with('.')
        || host.contains("..")
        || host
            .bytes()
            .any(|byte| !(byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.')))
    {
        return Err(NetworkError::InvalidHost(host));
    }
    for label in host.split('.') {
        if label.is_empty() || label.len() > 63 || label.starts_with('-') || label.ends_with('-') {
            return Err(NetworkError::InvalidHost(host));
        }
    }
    Ok(NormalizedHost {
        host,
        literal: None,
    })
}

/// Address ranges that must never be reachable through the broker, including the cloud metadata
/// endpoint, the owner's LAN and every transition mechanism that could reach them.
pub fn is_blocked_ip(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => is_blocked_v4(*ip),
        IpAddr::V6(ip) => is_blocked_v6(*ip),
    }
}

fn is_blocked_v4(ip: Ipv4Addr) -> bool {
    let value = u32::from(ip);
    ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_multicast()
        || ip == Ipv4Addr::BROADCAST
        || value >> 24 == 0
        || (value & 0xffff_0000) == 0xc0a8_0000
        || (value & 0xffc0_0000) == 0x6440_0000
        || (value & 0xfffe_0000) == 0xc612_0000
        || (value & 0xffff_ff00) == 0xc000_0200
        || (value & 0xffff_ff00) == 0xc633_6400
        || (value & 0xffff_ff00) == 0xcb00_7100
        || value >= 0xf000_0000
}

fn is_blocked_v6(ip: Ipv6Addr) -> bool {
    if let Some(v4) = ip.to_ipv4_mapped() {
        return is_blocked_v4(v4);
    }
    let segments = ip.segments();
    ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_multicast()
        || (segments[0] & 0xfe00) == 0xfc00
        || (segments[0] & 0xffc0) == 0xfe80
        || (segments[0] == 0x2001 && segments[1] == 0x0db8)
        || (segments[0] == 0x2001 && segments[1] == 0x0000)
        || (segments[0] == 0x0064 && segments[1] == 0xff9b && segments[2..6] == [0, 0, 0, 0])
        || (segments[0] == 0x2002)
}

#[derive(Debug)]
pub struct NetworkGrant {
    pub id: Uuid,
    pub principal: SubsystemId,
    pub job_id: Uuid,
    pub policy: NetworkPolicyRef,
    pub issued_at_ms: i64,
    pub expires_at_ms: i64,
    revoked: AtomicBool,
    dns_queries: std::sync::atomic::AtomicU32,
}

impl NetworkGrant {
    pub fn is_revoked(&self) -> bool {
        self.revoked.load(Ordering::Acquire)
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct BindingToken(String);

impl BindingToken {
    pub fn expose_secret(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for BindingToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[REDACTED_NETWORK_BINDING]")
    }
}

impl fmt::Display for BindingToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[REDACTED_NETWORK_BINDING]")
    }
}

/// Versioned policy store plus job-bound grants. The authority itself is Guardian; this struct
/// only enforces a grant that was already issued by the trusted supervisor.
type ActiveConnections = Mutex<HashMap<Uuid, Vec<(u64, watch::Sender<bool>)>>>;

pub struct NetworkBroker {
    policies: RwLock<BTreeMap<(String, u64), Arc<NetworkPolicy>>>,
    current: RwLock<HashMap<String, u64>>,
    grants: RwLock<HashMap<Uuid, Arc<NetworkGrant>>>,
    bindings: RwLock<HashMap<[u8; 32], Uuid>>,
    resolver: Arc<dyn Resolver>,
    active: ActiveConnections,
    next_connection: std::sync::atomic::AtomicU64,
}

impl NetworkBroker {
    pub fn new(resolver: Arc<dyn Resolver>) -> Self {
        Self {
            policies: RwLock::new(BTreeMap::new()),
            current: RwLock::new(HashMap::new()),
            grants: RwLock::new(HashMap::new()),
            bindings: RwLock::new(HashMap::new()),
            resolver,
            active: Mutex::new(HashMap::new()),
            next_connection: std::sync::atomic::AtomicU64::new(1),
        }
    }

    /// Versions are monotonic per policy id. Older in-flight grants keep their version; a grant
    /// can never be silently retargeted to a newer policy.
    pub fn install_policy(&self, policy: NetworkPolicy) -> Result<(), NetworkError> {
        policy.validate()?;
        let key = (policy.id.clone(), policy.version);
        let mut current = self.current.write().unwrap();
        if let Some(version) = current.get(&policy.id)
            && (policy.version < *version
                || (policy.version == *version
                    && self
                        .policies
                        .read()
                        .unwrap()
                        .get(&key)
                        .is_some_and(|existing| **existing != policy)))
        {
            return Err(NetworkError::PolicyVersion);
        }
        self.policies
            .write()
            .unwrap()
            .insert(key, Arc::new(policy.clone()));
        current.insert(policy.id.clone(), policy.version);
        Ok(())
    }

    pub fn current_version(&self, policy_id: &str) -> Option<u64> {
        self.current.read().unwrap().get(policy_id).copied()
    }

    pub fn issue_grant(
        &self,
        principal: SubsystemId,
        job_id: Uuid,
        policy_id: &str,
        ttl_ms: i64,
        now_ms: i64,
    ) -> Result<(Arc<NetworkGrant>, BindingToken), NetworkError> {
        if job_id.is_nil() || !(1..=MAX_GRANT_TTL_MS).contains(&ttl_ms) {
            return Err(NetworkError::InvalidPolicy("grant".into()));
        }
        let version = self
            .current_version(policy_id)
            .ok_or(NetworkError::PolicyUnavailable)?;
        let token = generate_token()?;
        let grant = Arc::new(NetworkGrant {
            id: Uuid::new_v4(),
            principal,
            job_id,
            policy: NetworkPolicyRef {
                policy_id: policy_id.into(),
                policy_version: version,
            },
            issued_at_ms: now_ms,
            expires_at_ms: now_ms + ttl_ms,
            revoked: AtomicBool::new(false),
            dns_queries: std::sync::atomic::AtomicU32::new(0),
        });
        self.grants.write().unwrap().insert(grant.id, grant.clone());
        self.bindings
            .write()
            .unwrap()
            .insert(hash_token(&token), grant.id);
        Ok((grant, token))
    }

    pub fn revoke(&self, grant_id: Uuid) -> bool {
        let Some(grant) = self.grants.read().unwrap().get(&grant_id).cloned() else {
            return false;
        };
        grant.revoked.store(true, Ordering::Release);
        // Keep the binding so a revoked token reports the revocation rather than silently
        // becoming an unknown token. The grant stays until the session is swept.

        if let Some(active) = self.active.lock().unwrap().remove(&grant_id) {
            for (_, sender) in active {
                let _ = sender.send(true);
            }
        }
        true
    }

    /// Resolve and authorize a proxied destination. The returned addresses are the only ones the
    /// caller may connect to.
    pub async fn authorize(
        &self,
        binding: &str,
        host: &str,
        port: u16,
        protocol: ProxyProtocol,
        now_ms: i64,
    ) -> Result<AllowedDestination, NetworkError> {
        let grant = self.grant_for(binding, now_ms)?;
        let policy = self
            .policies
            .read()
            .unwrap()
            .get(&(grant.policy.policy_id.clone(), grant.policy.policy_version))
            .cloned()
            .ok_or(NetworkError::PolicyUnavailable)?;
        if policy.version != grant.policy.policy_version {
            return Err(NetworkError::PolicyVersion);
        }
        if grant.dns_queries.fetch_add(1, Ordering::AcqRel) >= policy.limits.max_dns_queries {
            return Err(NetworkError::Denied(
                "DNS query budget exhausted for this grant".into(),
            ));
        }
        match authorize_connect(&policy, host, port, protocol, self.resolver.as_ref()).await {
            NetworkDecision::Allowed(destination) => Ok(destination),
            NetworkDecision::Denied(violation) => Err(NetworkError::Denied(violation.to_string())),
        }
    }

    pub fn grant_for(&self, binding: &str, now_ms: i64) -> Result<Arc<NetworkGrant>, NetworkError> {
        let id = self
            .bindings
            .read()
            .unwrap()
            .get(&hash_token_str(binding))
            .copied()
            .ok_or(NetworkError::GrantNotFound)?;
        let grant = self
            .grants
            .read()
            .unwrap()
            .get(&id)
            .cloned()
            .ok_or(NetworkError::GrantNotFound)?;
        if grant.is_revoked() {
            return Err(NetworkError::GrantRevoked);
        }
        if grant.expires_at_ms <= now_ms {
            return Err(NetworkError::GrantExpired);
        }
        Ok(grant)
    }

    /// Register a cancellable proxy connection. Revocation notifies every registered sender.
    pub fn register_connection(&self, grant_id: Uuid) -> (u64, watch::Receiver<bool>) {
        let id = self.next_connection.fetch_add(1, Ordering::AcqRel);
        let (sender, receiver) = watch::channel(false);
        self.active
            .lock()
            .unwrap()
            .entry(grant_id)
            .or_default()
            .push((id, sender));
        (id, receiver)
    }

    pub fn unregister_connection(&self, grant_id: Uuid, connection_id: u64) {
        if let Some(active) = self.active.lock().unwrap().get_mut(&grant_id) {
            active.retain(|(id, _)| *id != connection_id);
        }
    }

    pub fn active_connections(&self, grant_id: Uuid) -> usize {
        self.active
            .lock()
            .unwrap()
            .get(&grant_id)
            .map(Vec::len)
            .unwrap_or(0)
    }

    pub fn policy_max_connections(&self, policy_id: &str, version: u64) -> Option<u32> {
        self.policies
            .read()
            .unwrap()
            .get(&(policy_id.to_owned(), version))
            .map(|policy| policy.limits.max_connections)
    }

    /// Byte/bandwidth/idle ceilings for an active grant. The proxy applies them to the tunnel.
    pub fn policy_limits(&self, policy_id: &str, version: u64) -> Option<(u64, u64, u64, u64)> {
        self.policies
            .read()
            .unwrap()
            .get(&(policy_id.to_owned(), version))
            .map(|policy| {
                (
                    policy.limits.max_bytes_per_connection,
                    policy.limits.max_bps,
                    policy.limits.idle_timeout_ms,
                    policy.limits.connect_timeout_ms,
                )
            })
    }
}

fn generate_token() -> Result<BindingToken, NetworkError> {
    let mut bytes = [0u8; 32];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| NetworkError::Io("system random unavailable".into()))?;
    let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    Ok(BindingToken(format!("v1.{hex}")))
}

fn hash_token(token: &BindingToken) -> [u8; 32] {
    hash_token_str(&token.0)
}

fn hash_token_str(token: &str) -> [u8; 32] {
    let digest = ring::digest::digest(&ring::digest::SHA256, token.as_bytes());
    let mut hash = [0u8; 32];
    hash.copy_from_slice(digest.as_ref());
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FixedResolver(Vec<IpAddr>);

    #[async_trait]
    impl Resolver for FixedResolver {
        async fn resolve(&self, _host: &str, _port: u16) -> Result<Vec<IpAddr>, NetworkError> {
            Ok(self.0.clone())
        }
    }

    fn rule(effect: Effect, host: HostPattern, port: u16, protocol: ProxyProtocol) -> NetworkRule {
        NetworkRule {
            effect,
            host,
            ports: vec![port],
            protocols: vec![protocol],
            reason: String::new(),
        }
    }

    fn public() -> IpAddr {
        "93.184.216.34".parse().unwrap()
    }

    #[test]
    fn wildcards_match_strict_subdomains_only() {
        assert!(HostPattern::Subdomain("example.com".into()).matches("api.example.com"));
        assert!(!HostPattern::Subdomain("example.com".into()).matches("example.com"));
        assert!(!HostPattern::Subdomain("example.com".into()).matches("badexample.com"));
        assert!(HostPattern::Exact("example.com".into()).matches("example.com"));
    }

    #[test]
    fn host_normalization_rejects_ambiguous_names() {
        assert_eq!(
            normalize_host("API.Example.COM.").unwrap().host,
            "api.example.com"
        );
        assert!(normalize_host("api..example.com").is_err());
        assert!(normalize_host("-api.example.com").is_err());
        assert!(normalize_host("api.example.com:443").is_err());
        assert!(normalize_host("exämple.com").is_err());
        assert!(normalize_host("").is_err());
    }

    #[test]
    fn deny_precedence_beats_a_broader_allow() {
        let policy = NetworkPolicy::new(
            "p",
            1,
            vec![
                rule(
                    Effect::Allow,
                    HostPattern::Subdomain("example.com".into()),
                    443,
                    ProxyProtocol::Https,
                ),
                rule(
                    Effect::Deny,
                    HostPattern::Exact("secrets.example.com".into()),
                    443,
                    ProxyProtocol::Https,
                ),
            ],
            NetworkLimits::default(),
        );
        assert!(
            policy
                .evaluate("api.example.com", 443, ProxyProtocol::Https)
                .is_ok()
        );
        assert!(matches!(
            policy.evaluate("secrets.example.com", 443, ProxyProtocol::Https),
            Err(NetworkDecision::Denied(_))
        ));
        assert!(matches!(
            policy.evaluate("other.test", 443, ProxyProtocol::Https),
            Err(NetworkDecision::Denied(_))
        ));
    }

    #[test]
    fn private_and_metadata_ranges_are_blocked() {
        for value in [
            "127.0.0.1",
            "10.0.0.1",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "0.0.0.0",
            "224.0.0.1",
            "::1",
            "fe80::1",
            "fc00::1",
            "ff02::1",
            "2001:db8::1",
            "::ffff:127.0.0.1",
            "64:ff9b::7f00:1",
            "2002:7f00:1::",
        ] {
            let ip: IpAddr = value.parse().unwrap();
            assert!(is_blocked_ip(&ip), "{value} should be blocked");
        }
        assert!(!is_blocked_ip(&public()));
    }

    #[tokio::test]
    async fn a_public_destination_is_allowed_and_a_rebinding_answer_is_not() {
        let policy = NetworkPolicy::new(
            "p",
            1,
            vec![rule(
                Effect::Allow,
                HostPattern::Exact("example.com".into()),
                443,
                ProxyProtocol::Https,
            )],
            NetworkLimits::default(),
        );
        let allowed = authorize_connect(
            &policy,
            "example.com",
            443,
            ProxyProtocol::Https,
            &FixedResolver(vec![public()]),
        )
        .await;
        assert!(matches!(allowed, NetworkDecision::Allowed(_)));

        let mixed = authorize_connect(
            &policy,
            "example.com",
            443,
            ProxyProtocol::Https,
            &FixedResolver(vec![public(), "127.0.0.1".parse().unwrap()]),
        )
        .await;
        assert!(matches!(mixed, NetworkDecision::Denied(_)));
    }

    #[tokio::test]
    async fn grants_expire_and_revoke() {
        let now = 1_800_000_000_000;
        let broker = NetworkBroker::new(Arc::new(FixedResolver(vec![public()])));
        broker
            .install_policy(NetworkPolicy::new(
                "registry",
                1,
                vec![rule(
                    Effect::Allow,
                    HostPattern::Exact("example.com".into()),
                    443,
                    ProxyProtocol::Https,
                )],
                NetworkLimits::default(),
            ))
            .unwrap();
        let (grant, token) = broker
            .issue_grant(SubsystemId::Forge, Uuid::new_v4(), "registry", 60_000, now)
            .unwrap();
        assert!(
            broker
                .authorize(
                    token.expose_secret(),
                    "example.com",
                    443,
                    ProxyProtocol::Https,
                    now
                )
                .await
                .is_ok()
        );
        assert_eq!(
            broker
                .authorize(
                    token.expose_secret(),
                    "example.com",
                    443,
                    ProxyProtocol::Https,
                    now + 60_001
                )
                .await,
            Err(NetworkError::GrantExpired)
        );
        assert!(broker.revoke(grant.id));
        assert_eq!(
            broker
                .authorize(
                    token.expose_secret(),
                    "example.com",
                    443,
                    ProxyProtocol::Https,
                    now
                )
                .await,
            Err(NetworkError::GrantRevoked)
        );
    }

    #[tokio::test]
    async fn stale_policy_versions_are_rejected() {
        let broker = NetworkBroker::new(Arc::new(FixedResolver(vec![public()])));
        broker
            .install_policy(NetworkPolicy::new("p", 2, vec![], NetworkLimits::default()))
            .unwrap();
        assert_eq!(
            broker.install_policy(NetworkPolicy::new("p", 1, vec![], NetworkLimits::default())),
            Err(NetworkError::PolicyVersion)
        );
        broker
            .install_policy(NetworkPolicy::new("p", 3, vec![], NetworkLimits::default()))
            .unwrap();
        assert_eq!(broker.current_version("p"), Some(3));
    }

    #[tokio::test]
    async fn a_loopback_literal_is_denied_even_when_a_rule_matches() {
        let policy = NetworkPolicy::new(
            "p",
            1,
            vec![rule(
                Effect::Allow,
                HostPattern::Exact("127.0.0.1".into()),
                443,
                ProxyProtocol::Https,
            )],
            NetworkLimits::default(),
        );
        let decision = authorize_connect(
            &policy,
            "127.0.0.1",
            443,
            ProxyProtocol::Https,
            &FixedResolver(vec![]),
        )
        .await;
        assert!(matches!(decision, NetworkDecision::Denied(_)));
    }
}
