//! Pure tensor placement across the host GPU/RAM and the lending PC's RPC devices.
use crate::heavy::gguf::GgufSummary;
use std::collections::BTreeMap;

/// One device `llama-server --list-devices` reported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListedDevice {
    pub name: String,
    pub description: String,
    pub free_bytes: u64,
}

/// Parse `llama-server --list-devices`; unknown lines are skipped.
///
/// Verified on build 12014:
/// `  Vulkan0: AMD Radeon RX 5500 XT (RADV NAVI14) (8176 MiB, 8150 MiB free)`
/// `  RPC0: 127.0.0.1:50199 (30702 MiB, 30702 MiB free)`
pub fn parse_list_devices(stdout: &str) -> Vec<ListedDevice> {
    let mut devices = Vec::new();
    for line in stdout.lines() {
        let Some(colon) = line.find(':') else {
            continue;
        };
        let name = line[..colon].trim();
        if name.is_empty() || name.contains(' ') || name.eq_ignore_ascii_case("available devices") {
            continue;
        }
        let Some(free_bytes) = free_bytes(line) else {
            continue;
        };
        let Some(description_end) = line.rfind(" (") else {
            continue;
        };
        let description = line[colon + 1..description_end].trim();
        if description.is_empty() {
            continue;
        }
        devices.push(ListedDevice {
            name: name.to_owned(),
            description: description.to_owned(),
            free_bytes,
        });
    }
    devices
}

/// The `(total MiB, free MiB free)` group at the end of a listed device line.
fn free_bytes(line: &str) -> Option<u64> {
    let close = line.rfind(" free)")?;
    let open = line[..close].rfind('(')?;
    let inner = &line[open + 1..close];
    let (_, free) = inner.split_once(',')?;
    let mib: u64 = free.split_whitespace().next()?.parse().ok()?;
    mib.checked_mul(1024 * 1024)
}

/// What the owner and the worker allow, and the host's own share.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Budgets {
    /// Host GPU device name (e.g. "Vulkan0") and its cap; cap 0 = host GPU off.
    pub host_gpu: Option<(String, u64)>,
    pub host_ram_bytes: u64,
    /// Remote devices in preference order (PC GPU first, then PC CPU), name as listed, with their budget.
    pub remote: Vec<(String, u64)>,
    /// KV cache + compute buffers reserved on the device holding attention (from ctx size;
    /// fixed a priori formula documented by the lane).
    pub kv_bytes: u64,
}

/// Where the planner put everything, plus the llama-server arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Placement {
    /// Args to append: `--device <list>` (omitted when every device is CPU),
    /// `--tensor-split 1,0[,0...]` when the host GPU holds the attention share (its
    /// `dev_layer` stays `Vulkan0`, so KV and non-experts sit there too), `-ngl 999`,
    /// `--no-kv-offload` when the host GPU share is off (KV follows `dev_layer`, not `-ot`),
    /// one `-ot <regex>=<device>` per expert group, then a final `-ot .*=<host device>` so
    /// llama.cpp keeps every non-expert weight on the host share. The host catch-all must stay
    /// last: llama.cpp applies the first matching override.
    pub args: Vec<String>,
    pub host_ram_bytes: u64,
    pub host_vram_bytes: u64,
    pub remote_bytes: u64,
    pub per_device: Vec<(String, u64)>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PlanError {
    #[error("the model needs {needed} bytes but only {available} are available")]
    DoesNotFit { needed: u64, available: u64 },
    #[error("the model has no expert tensors")]
    NoExperts,
}

/// Plan where every tensor of `model` goes.
///
/// Rules, in order:
/// 1. All non-expert tensors and `kv_bytes` go on the host GPU if they fit its cap, else in
///    host RAM.
/// 2. Expert tensors (`blk.N.ffn_<lowercase>_exps.*`) fill the remote devices whole layers at
///    a time, in their configured preference order.
/// 3. Any layers left over go to host RAM, up to `host_ram_bytes`.
/// 4. Anything still left over is [`PlanError::DoesNotFit`].
///
/// The function is pure and deterministic: it reads only its arguments.
pub fn plan(model: &GgufSummary, budgets: &Budgets) -> Result<Placement, PlanError> {
    let mut host_tensors = 0u64;
    let mut layers: BTreeMap<u32, u64> = BTreeMap::new();
    for tensor in &model.tensors {
        match expert_layer(&tensor.name) {
            Some(layer) => *layers.entry(layer).or_default() += tensor.bytes,
            None => host_tensors = host_tensors.saturating_add(tensor.bytes),
        }
    }
    if layers.is_empty() {
        return Err(PlanError::NoExperts);
    }

    // Rule 1: non-experts and KV go on the host GPU when they fit, else in host RAM.
    let host_attention = host_tensors.saturating_add(budgets.kv_bytes);
    let (host_vram_bytes, mut host_ram_bytes, host_gpu_name) = match &budgets.host_gpu {
        Some((name, cap)) if *cap > 0 && host_attention > 0 && host_attention <= *cap => {
            (host_attention, 0, Some(name.clone()))
        }
        _ => (0, host_attention, None),
    };
    if host_ram_bytes > budgets.host_ram_bytes {
        let available = if let Some((_, cap)) = &budgets.host_gpu {
            budgets.host_ram_bytes.max(*cap)
        } else {
            budgets.host_ram_bytes
        };
        return Err(PlanError::DoesNotFit {
            needed: host_attention,
            available,
        });
    }

    // Rule 2: whole expert layers fill the remote devices in order.
    let mut remote_used = vec![0u64; budgets.remote.len()];
    let mut remote_layers: Vec<Vec<u32>> = vec![Vec::new(); budgets.remote.len()];
    let mut remote_index = 0usize;
    let mut host_layers: Vec<(u32, u64)> = Vec::new();
    for (layer, bytes) in &layers {
        let mut placed = false;
        while remote_index < budgets.remote.len() {
            let (_, cap) = &budgets.remote[remote_index];
            if remote_used[remote_index].saturating_add(*bytes) <= *cap {
                remote_used[remote_index] += *bytes;
                remote_layers[remote_index].push(*layer);
                placed = true;
                break;
            }
            remote_index += 1;
        }
        if !placed {
            host_layers.push((*layer, *bytes));
        }
    }

    // Rule 3: leftovers go to host RAM; rule 4: otherwise DoesNotFit.
    let mut host_layer_numbers = Vec::new();
    for (layer, bytes) in host_layers {
        let next = host_ram_bytes.saturating_add(bytes);
        if next > budgets.host_ram_bytes {
            return Err(PlanError::DoesNotFit {
                needed: bytes,
                available: budgets.host_ram_bytes.saturating_sub(host_ram_bytes),
            });
        }
        host_ram_bytes = next;
        host_layer_numbers.push(layer);
    }

    let mut devices = Vec::new();
    if let Some(name) = &host_gpu_name {
        devices.push(name.clone());
    }
    for (index, used) in remote_used.iter().enumerate() {
        if *used > 0 {
            devices.push(budgets.remote[index].0.clone());
        }
    }
    let mut args = Vec::new();
    // llama.cpp rejects CPU in --device (`common/arg.cpp`, parse_device_list). CPU is always
    // available for `-ot =CPU`, so it never needs to be listed.
    if !devices.is_empty() {
        args.push("--device".to_owned());
        args.push(devices.join(","));
        if host_gpu_name.is_some() && host_vram_bytes > 0 {
            // The host GPU is first in `devices`; a share of 1 and zeros for the RPC devices
            // puts every layer's dev_layer on it, so KV follows the attention share.
            let mut split = String::from("1");
            for _ in 1..devices.len() {
                split.push_str(",0");
            }
            args.push("--tensor-split".to_owned());
            args.push(split);
        }
    }
    args.push("-ngl".to_owned());
    args.push("999".to_owned());
    if host_vram_bytes == 0 {
        // llama.cpp allocates offloaded KV on the layer's device, not by `-ot`. With no host
        // GPU share, keep KV in host RAM instead of following layers onto an RPC device.
        args.push("--no-kv-offload".to_owned());
    }
    for (index, layers) in remote_layers.iter().enumerate() {
        for run in contiguous_runs(layers) {
            args.push("-ot".to_owned());
            args.push(override_pattern(&run, &budgets.remote[index].0));
        }
    }
    for run in contiguous_runs(&host_layer_numbers) {
        args.push("-ot".to_owned());
        args.push(override_pattern(&run, "CPU"));
    }
    // Everything not overridden above stays on the host share. First match wins in
    // llama.cpp, so this catch-all must come after the expert groups.
    let host_device = host_gpu_name.as_deref().unwrap_or("CPU");
    args.push("-ot".to_owned());
    args.push(format!(".*={host_device}"));

    let mut per_device = Vec::new();
    if let Some(name) = &host_gpu_name
        && host_vram_bytes > 0
    {
        per_device.push((name.clone(), host_vram_bytes));
    }
    let mut remote_bytes = 0u64;
    for (index, used) in remote_used.iter().enumerate() {
        if *used > 0 {
            remote_bytes = remote_bytes.saturating_add(*used);
            per_device.push((budgets.remote[index].0.clone(), *used));
        }
    }
    if host_ram_bytes > 0 {
        per_device.push(("CPU".into(), host_ram_bytes));
    }

    Ok(Placement {
        args,
        host_ram_bytes,
        host_vram_bytes,
        remote_bytes,
        per_device,
    })
}

/// The layer number of an expert tensor like `blk.7.ffn_up_exps.weight`.
fn expert_layer(name: &str) -> Option<u32> {
    let rest = name.strip_prefix("blk.")?;
    let (digits, rest) = rest.split_once('.')?;
    let layer = digits.parse::<u32>().ok()?;
    is_expert_tensor(rest).then_some(layer)
}

/// `ffn_[a-z_]*_exps\.` — the same shape the `-ot` pattern uses, so a fused name such as
/// `ffn_gate_up_exps` is counted and overridden together.
fn is_expert_tensor(rest: &str) -> bool {
    let Some(tail) = rest.strip_prefix("ffn_") else {
        return false;
    };
    let Some(end) = tail.find("_exps.") else {
        return false;
    };
    tail[..end]
        .chars()
        .all(|c| c.is_ascii_lowercase() || c == '_')
}

/// Group ascending layer numbers into contiguous runs.
fn contiguous_runs(layers: &[u32]) -> Vec<Vec<u32>> {
    let mut runs: Vec<Vec<u32>> = Vec::new();
    for layer in layers {
        let extend = runs
            .last()
            .and_then(|run| run.last())
            .is_some_and(|last| last.checked_add(1) == Some(*layer));
        if extend {
            if let Some(run) = runs.last_mut() {
                run.push(*layer);
            }
        } else {
            runs.push(vec![*layer]);
        }
    }
    runs
}

/// `blk\.(0|1|2)\.ffn_[a-z_]*_exps\.=RPC0`
fn override_pattern(layers: &[u32], device: &str) -> String {
    let alternatives = layers
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join("|");
    format!(r"blk\.({alternatives})\.ffn_[a-z_]*_exps\.={device}")
}
