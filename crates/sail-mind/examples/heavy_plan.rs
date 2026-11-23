//! Plan a heavy MoE model's tensor placement without loading its weights.
//!
//! ```text
//! heavy_plan <model.gguf> --host-gpu Vulkan0:2.5 --host-ram 6 \
//!     --remote RPC0:14.5 --remote RPC1:46 --ctx 16384
//! ```
//!
//! Sizes are GiB. `--host-gpu NAME:0` keeps the host GPU off. The KV budget uses the
//! measured gpt-oss-120b shape (eight KV heads, width 64, two planes, two bytes per f16
//! element per block per token) plus a fixed 512 MiB compute reserve; this example is for
//! that model file. Reading the header must not touch tensor data.
use sail_mind::heavy::gguf::read_summary;
use sail_mind::heavy::placement::{Budgets, Placement, PlanError, plan};
use std::path::PathBuf;
use std::process::ExitCode;

const GIB: u64 = 1 << 30;
const MIB: u64 = 1 << 20;
/// 8 heads x 64 width x 2 planes x 2 bytes, measured from the gpt-oss-120b header.
const GPT_OSS_KV_BYTES_PER_BLOCK_TOKEN: u64 = 8 * 64 * 2 * 2;
/// Fixed a priori: the model's compute buffers.
const COMPUTE_BUFFER_BYTES: u64 = 512 * MIB;

const USAGE: &str = "\
usage: heavy_plan <model.gguf> --host-gpu NAME:GIB --host-ram GIB \
--remote NAME:GIB [--remote NAME:GIB ...] --ctx TOKENS

  --host-gpu NAME:GIB  host GPU device and its cap; 0 keeps the host GPU off
  --host-ram GIB       host RAM cap for the non-expert share and leftover layers
  --remote NAME:GIB    lending PC device and its budget; repeat in preference order
  --ctx TOKENS         context length used for the KV budget
";

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(error) => {
            eprintln!("heavy_plan: {error}\n\n{USAGE}");
            ExitCode::from(2)
        }
    }
}

fn run() -> Result<ExitCode, String> {
    let options = Options::parse()?;
    let summary = read_summary(&options.model).map_err(|error| error.to_string())?;
    let kv_bytes = options
        .ctx
        .saturating_mul(u64::from(summary.block_count))
        .saturating_mul(GPT_OSS_KV_BYTES_PER_BLOCK_TOKEN)
        .saturating_add(COMPUTE_BUFFER_BYTES);
    let budgets = Budgets {
        host_gpu: options.host_gpu.clone(),
        host_ram_bytes: options.host_ram,
        remote: options.remote.clone(),
        kv_bytes,
    };

    println!(
        "model: {} ({} blocks, {} experts, {} tensors)",
        summary.architecture,
        summary.block_count,
        summary
            .expert_count
            .map_or_else(|| "no".to_owned(), |count| count.to_string()),
        summary.tensors.len()
    );
    println!(
        "budgets: host RAM {}; host GPU {}; {}; ctx {} tokens -> KV + compute {}",
        gib(options.host_ram),
        options.host_gpu.as_ref().map_or_else(
            || "off".to_owned(),
            |(name, cap)| format!("{name} {}", gib(*cap))
        ),
        options
            .remote
            .iter()
            .map(|(name, cap)| format!("{name} {}", gib(*cap)))
            .collect::<Vec<_>>()
            .join(", "),
        options.ctx,
        gib(kv_bytes)
    );

    match plan(&summary, &budgets) {
        Ok(placement) => {
            print_placement(&placement);
            Ok(ExitCode::SUCCESS)
        }
        Err(PlanError::DoesNotFit { needed, available }) => {
            println!(
                "Does not fit: needs {}, but only {} is available.",
                gib(needed),
                gib(available)
            );
            Ok(ExitCode::from(1))
        }
        Err(PlanError::NoExperts) => {
            println!("Does not fit: this model has no expert tensors for the pooled PC to hold.");
            Ok(ExitCode::from(1))
        }
    }
}

fn print_placement(placement: &Placement) {
    println!("args: {}", placement.args.join(" "));
    println!("per device:");
    for (name, bytes) in &placement.per_device {
        println!("  {name}: {}", gib(*bytes));
    }
    println!(
        "host RAM {}, host VRAM {}, remote {}",
        gib(placement.host_ram_bytes),
        gib(placement.host_vram_bytes),
        gib(placement.remote_bytes)
    );
}

fn gib(bytes: u64) -> String {
    format!("{:.2} GiB", bytes as f64 / GIB as f64)
}

struct Options {
    model: PathBuf,
    host_gpu: Option<(String, u64)>,
    host_ram: u64,
    remote: Vec<(String, u64)>,
    ctx: u64,
}

impl Options {
    fn parse() -> Result<Self, String> {
        let mut model = None;
        let mut host_gpu = None;
        let mut host_ram = None;
        let mut remote = Vec::new();
        let mut ctx = None;
        let mut args = std::env::args().skip(1);
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--host-gpu" => {
                    host_gpu = Some(parse_device(&next(&mut args, "--host-gpu")?)?);
                }
                "--host-ram" => {
                    host_ram = Some(parse_gib(&next(&mut args, "--host-ram")?)?);
                }
                "--remote" => remote.push(parse_device(&next(&mut args, "--remote")?)?),
                "--ctx" => {
                    let text = next(&mut args, "--ctx")?;
                    let value = text
                        .parse::<u64>()
                        .map_err(|_| format!("--ctx needs a whole token count, got {text}"))?;
                    if value == 0 {
                        return Err("--ctx must be at least 1".into());
                    }
                    ctx = Some(value);
                }
                "-h" | "--help" => return Err("help requested".into()),
                other if other.starts_with("--") => {
                    return Err(format!("unknown flag {other}"));
                }
                other => {
                    if model.is_some() {
                        return Err(format!("unexpected extra model path {other}"));
                    }
                    model = Some(PathBuf::from(other));
                }
            }
        }
        let options = Self {
            model: model.ok_or("missing the GGUF path")?,
            host_gpu,
            host_ram: host_ram.ok_or("missing --host-ram")?,
            remote,
            ctx: ctx.ok_or("missing --ctx")?,
        };
        if options.remote.is_empty() {
            return Err("give at least one --remote device".into());
        }
        Ok(options)
    }
}

fn next(args: &mut impl Iterator<Item = String>, flag: &str) -> Result<String, String> {
    args.next().ok_or_else(|| format!("{flag} needs a value"))
}

fn parse_device(spec: &str) -> Result<(String, u64), String> {
    let (name, size) = spec
        .rsplit_once(':')
        .ok_or_else(|| format!("expected NAME:GIB, got {spec}"))?;
    if name.is_empty() || name.chars().any(|c| c.is_whitespace() || c == ',') {
        return Err(format!("invalid device name {name}"));
    }
    Ok((name.to_owned(), parse_gib(size)?))
}

fn parse_gib(text: &str) -> Result<u64, String> {
    let value = text
        .parse::<f64>()
        .map_err(|_| format!("expected a size in GiB, got {text}"))?;
    if !value.is_finite() || value < 0.0 {
        return Err(format!("expected a non-negative size in GiB, got {text}"));
    }
    let bytes = value * GIB as f64;
    if bytes > u64::MAX as f64 {
        return Err(format!("size {text} GiB is too large"));
    }
    Ok(bytes.round() as u64)
}
