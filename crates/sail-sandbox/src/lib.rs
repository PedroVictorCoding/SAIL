//! Fail-closed sandbox contract shared by Guardian, the command runner and Forge.
//!
//! This crate defines the typed [`plan::ExecutionPlan`], checked [`policy::PolicyProfile`]
//! defaults and the default-deny [`network::NetworkBroker`] authorization core. It deliberately
//! does not spawn processes or open sockets in its library API except through
//! [`proxy::serve`], which only tunnels an already-authorized CONNECT request.
//!
//! A plan is data. It is not a grant, and deserializing one never authorizes execution or
//! network access. Guardian remains the authority; the runner remains the single executor.
pub mod network;
pub mod plan;
pub mod policy;
pub mod proxy;

pub use network::{
    AllowedDestination, BindingToken, HostPattern, NetworkBroker, NetworkDecision, NetworkError,
    NetworkGrant, NetworkLimits, NetworkPolicy, NetworkPolicyRef, NetworkRule, NetworkViolation,
    ProxyProtocol, Resolver, SystemResolver, authorize_connect, is_blocked_ip,
};
pub use plan::{
    CredentialRef, ExecutableIdentity, ExecutionLimits, ExecutionPlan, MountRoot, PLAN_VERSION,
    PlanError,
};
pub use policy::{PolicyProfile, ProfilePolicy, default_network_policies};
pub use proxy::{ProxyConfig, ProxyReject, ProxyRequest, serve};
