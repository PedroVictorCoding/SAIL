//! The host's heavy model: a large GGUF placed across the host and a lending PC's RPC devices.
//!
//! [`gguf`] reads only a model's header, [`placement`] decides where every tensor goes,
//! [`admission`] checks the host caps, and [`lane`] runs the second llama-server. K6 provides
//! the contracts; Codex fills [`lane`].
pub mod admission;
pub mod gguf;
pub mod lane;
pub mod placement;

pub use admission::{HEADROOM_BYTES, HostCaps, admit};
pub use gguf::{GgufError, GgufSummary, GgufTensor, parse, read_summary};
pub use lane::{HeavyConfig, HeavyEndpoint, HeavyLane};
pub use placement::{Budgets, ListedDevice, Placement, PlanError, parse_list_devices, plan};
