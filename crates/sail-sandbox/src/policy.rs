//! Checked, versioned profiles. These are Rust defaults, not unrestricted YAML interpreted by a
//! model. A profile narrows a plan; it never grants a capability by itself.
use crate::network::{
    Effect, HostPattern, NetworkLimits, NetworkPolicy, NetworkPolicyRef, NetworkRule, ProxyProtocol,
};
use crate::plan::{ExecutionLimits, ExecutionPlan, MountRoot, path_within};

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyProfile {
    Inspect,
    BuildOffline,
    FetchDependencies,
    ResearchFetch,
    CodingNetworked,
    PrivilegedApproved,
}

impl PolicyProfile {
    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "inspect" => Self::Inspect,
            "build_offline" => Self::BuildOffline,
            "fetch_dependencies" => Self::FetchDependencies,
            "research_fetch" => Self::ResearchFetch,
            "coding_networked" => Self::CodingNetworked,
            "privileged_approved" => Self::PrivilegedApproved,
            _ => return None,
        })
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Inspect => "inspect",
            Self::BuildOffline => "build_offline",
            Self::FetchDependencies => "fetch_dependencies",
            Self::ResearchFetch => "research_fetch",
            Self::CodingNetworked => "coding_networked",
            Self::PrivilegedApproved => "privileged_approved",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfilePolicy {
    pub profile: PolicyProfile,
    pub read_roots: Vec<String>,
    pub write_roots: Vec<String>,
    pub network: Option<NetworkPolicyRef>,
    pub limits: ExecutionLimits,
    /// Guardian must have an explicit approval for this profile.
    pub requires_approval: bool,
    /// The plan must name every capability explicitly; the profile supplies no default network.
    pub explicit_only: bool,
}

impl ProfilePolicy {
    pub fn for_profile(profile: PolicyProfile) -> Self {
        let system = ["/usr", "/lib", "/lib64", "/bin", "/etc/ssl", "/etc/pki"];
        let (read_roots, write_roots, network, limits, requires_approval, explicit_only) =
            match profile {
                PolicyProfile::Inspect => (
                    system.iter().copied().chain(["/work"]).collect::<Vec<_>>(),
                    Vec::new(),
                    None,
                    limits(30_000, 10, 256 * 1024 * 1024, 16, 64, 8, 64 * 1024),
                    false,
                    false,
                ),
                PolicyProfile::BuildOffline => (
                    system.iter().copied().chain(["/cache"]).collect::<Vec<_>>(),
                    vec!["/work", "/tmp"],
                    None,
                    limits(120_000, 60, 1024 * 1024 * 1024, 32, 128, 64, 256 * 1024),
                    false,
                    false,
                ),
                PolicyProfile::FetchDependencies => (
                    system.iter().copied().chain(["/cache"]).collect::<Vec<_>>(),
                    vec!["/work", "/cache", "/tmp"],
                    Some(NetworkPolicyRef {
                        policy_id: "registry".into(),
                        policy_version: 1,
                    }),
                    limits(120_000, 60, 1024 * 1024 * 1024, 32, 128, 64, 256 * 1024),
                    false,
                    false,
                ),
                PolicyProfile::ResearchFetch => (
                    system.iter().copied().chain(["/work"]).collect::<Vec<_>>(),
                    vec!["/work/scratch", "/tmp"],
                    Some(NetworkPolicyRef {
                        policy_id: "research".into(),
                        policy_version: 1,
                    }),
                    limits(60_000, 30, 512 * 1024 * 1024, 16, 64, 32, 64 * 1024),
                    false,
                    false,
                ),
                PolicyProfile::CodingNetworked => (
                    system.iter().copied().chain(["/work"]).collect::<Vec<_>>(),
                    vec!["/work"],
                    Some(NetworkPolicyRef {
                        policy_id: "task".into(),
                        policy_version: 1,
                    }),
                    limits(120_000, 60, 1024 * 1024 * 1024, 32, 128, 64, 256 * 1024),
                    true,
                    false,
                ),
                PolicyProfile::PrivilegedApproved => (
                    system.to_vec(),
                    vec!["/work"],
                    None,
                    ExecutionLimits::default(),
                    true,
                    true,
                ),
            };
        Self {
            profile,
            read_roots: read_roots.into_iter().map(str::to_owned).collect(),
            write_roots: write_roots.into_iter().map(str::to_owned).collect(),
            network,
            limits,
            requires_approval,
            explicit_only,
        }
    }

    /// Check that a plan stays inside the profile's mounts, network reference and ceilings.
    /// This is a narrowing check, never an authorization.
    pub fn validate_plan(&self, plan: &ExecutionPlan) -> Result<(), String> {
        for root in &plan.read_roots {
            if !self
                .read_roots
                .iter()
                .any(|allowed| path_within(&root.path, allowed))
            {
                return Err(format!(
                    "{}: read root {} is not part of the profile",
                    self.profile.as_str(),
                    root.path
                ));
            }
        }
        for root in &plan.write_roots {
            if !self
                .write_roots
                .iter()
                .any(|allowed| path_within(&root.path, allowed))
            {
                return Err(format!(
                    "{}: write root {} is not part of the profile",
                    self.profile.as_str(),
                    root.path
                ));
            }
        }
        if !self.explicit_only && plan.network != self.network {
            return Err(format!(
                "{}: network policy does not match the profile",
                self.profile.as_str()
            ));
        }
        if self.profile != PolicyProfile::PrivilegedApproved {
            let ceiling = &self.limits;
            let actual = &plan.limits;
            if actual.wall_ms > ceiling.wall_ms
                || actual.cpu_seconds > ceiling.cpu_seconds
                || actual.memory_bytes > ceiling.memory_bytes
                || actual.pids > ceiling.pids
                || actual.fds > ceiling.fds
                || actual.scratch_bytes > ceiling.scratch_bytes
                || actual.output_bytes > ceiling.output_bytes
                || actual.network.max_connections > ceiling.network.max_connections
                || actual.network.max_bytes_per_connection
                    > ceiling.network.max_bytes_per_connection
                || actual.network.max_bps > ceiling.network.max_bps
                || actual.network.connect_timeout_ms > ceiling.network.connect_timeout_ms
                || actual.network.idle_timeout_ms > ceiling.network.idle_timeout_ms
                || actual.network.max_dns_queries > ceiling.network.max_dns_queries
            {
                return Err(format!(
                    "{}: execution limits exceed the profile ceiling",
                    self.profile.as_str()
                ));
            }
        }
        Ok(())
    }

    pub fn mount_roots(&self) -> (Vec<MountRoot>, Vec<MountRoot>) {
        (
            self.read_roots
                .iter()
                .map(|path| MountRoot { path: path.clone() })
                .collect(),
            self.write_roots
                .iter()
                .map(|path| MountRoot { path: path.clone() })
                .collect(),
        )
    }
}

fn limits(
    wall_ms: u64,
    cpu_seconds: u64,
    memory_bytes: u64,
    pids: u32,
    fds: u32,
    scratch_mb: u64,
    output_bytes: usize,
) -> ExecutionLimits {
    ExecutionLimits {
        wall_ms,
        cpu_seconds,
        memory_bytes,
        pids,
        fds,
        scratch_bytes: scratch_mb * 1024 * 1024,
        output_bytes,
        network: NetworkLimits::default(),
    }
}

/// Built-in policies that profile defaults reference. Deployments may install newer versions;
/// a grant always keeps the version it was issued under.
pub fn default_network_policies() -> Vec<NetworkPolicy> {
    let registry = NetworkPolicy::new(
        "registry",
        1,
        vec![
            allow("crates.io", 443),
            allow("index.crates.io", 443),
            allow("static.crates.io", 443),
            subdomain("crates.io", 443),
        ],
        NetworkLimits::default(),
    );
    let research = NetworkPolicy::new("research", 1, Vec::new(), NetworkLimits::default());
    let task = NetworkPolicy::new("task", 1, Vec::new(), NetworkLimits::default());
    vec![registry, research, task]
}

fn allow(host: &str, port: u16) -> NetworkRule {
    NetworkRule {
        effect: Effect::Allow,
        host: HostPattern::Exact(host.into()),
        ports: vec![port],
        protocols: vec![ProxyProtocol::Https],
        reason: "built-in registry endpoint".into(),
    }
}

fn subdomain(host: &str, port: u16) -> NetworkRule {
    NetworkRule {
        effect: Effect::Allow,
        host: HostPattern::Subdomain(host.into()),
        ports: vec![port],
        protocols: vec![ProxyProtocol::Https],
        reason: "built-in registry subdomain".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn plan_for(profile: PolicyProfile) -> ExecutionPlan {
        let policy = ProfilePolicy::for_profile(profile);
        let (read_roots, write_roots) = policy.mount_roots();
        ExecutionPlan {
            version: crate::plan::PLAN_VERSION,
            job_id: Uuid::new_v4(),
            request_id: Uuid::new_v4(),
            principal: sail_common::types::SubsystemId::Forge,
            capability_grant_id: Uuid::new_v4(),
            executable: crate::plan::ExecutableIdentity {
                sandbox_path: "/usr/bin/cargo".into(),
                sha256: "a".repeat(64),
            },
            argv: vec!["check".into()],
            cwd: "/work".into(),
            read_roots,
            write_roots,
            network: policy.network.clone(),
            credentials: Vec::new(),
            limits: policy.limits,
            issued_at_ms: 1_800_000_000_000,
            expires_at_ms: 1_800_000_060_000,
            audit_correlation_id: Uuid::new_v4(),
        }
    }

    #[test]
    fn offline_profiles_never_grant_network() {
        for profile in [PolicyProfile::Inspect, PolicyProfile::BuildOffline] {
            let policy = ProfilePolicy::for_profile(profile);
            assert!(policy.network.is_none());
            let plan = plan_for(profile);
            plan.validate(1_800_000_000_001).unwrap();
            policy.validate_plan(&plan).unwrap();
        }
    }

    #[test]
    fn network_profiles_require_their_exact_policy() {
        let policy = ProfilePolicy::for_profile(PolicyProfile::FetchDependencies);
        let mut plan = plan_for(PolicyProfile::FetchDependencies);
        plan.validate(1_800_000_000_001).unwrap();
        policy.validate_plan(&plan).unwrap();
        plan.network = None;
        assert!(policy.validate_plan(&plan).is_err());
    }

    #[test]
    fn a_plan_cannot_widen_its_profile() {
        let policy = ProfilePolicy::for_profile(PolicyProfile::Inspect);
        let mut plan = plan_for(PolicyProfile::Inspect);
        plan.write_roots.push(MountRoot {
            path: "/work".into(),
        });
        assert!(policy.validate_plan(&plan).is_err());
    }

    #[test]
    fn built_in_registry_policy_is_bounded() {
        let policies = default_network_policies();
        let registry = policies.iter().find(|p| p.id == "registry").unwrap();
        registry.validate().unwrap();
        assert!(crate::network::is_blocked_ip(&"10.0.0.1".parse().unwrap()));
    }
}
