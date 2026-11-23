//! Pure placement for a gpt-oss-shaped MoE model, `--list-devices` parsing and admission.
use sail_mind::heavy::{
    admission::{AdmissionError, HEADROOM_BYTES, HostCaps, admit},
    gguf::{GgufSummary, GgufTensor},
    placement::{Budgets, ListedDevice, Placement, PlanError, kv_bytes, parse_list_devices, plan},
};

const GIB: u64 = 1 << 30;
const MIB: u64 = 1 << 20;

fn gpt_oss_120b() -> GgufSummary {
    let mut tensors = Vec::new();
    for layer in 0..36u32 {
        for (suffix, bytes) in [
            ("ffn_up_exps.weight", 2 * GIB),
            ("ffn_gate_exps.weight", 2 * GIB),
            ("ffn_down_exps.weight", 2 * GIB),
        ] {
            tensors.push(GgufTensor {
                name: format!("blk.{layer}.{suffix}"),
                bytes,
            });
        }
        tensors.push(GgufTensor {
            name: format!("blk.{layer}.ffn_up.weight"),
            bytes: 100 * MIB,
        });
    }
    tensors.push(GgufTensor {
        name: "token_embd.weight".into(),
        bytes: GIB,
    });
    GgufSummary {
        architecture: "gpt-oss".into(),
        block_count: 36,
        expert_count: Some(128),
        head_count_kv: Some(8),
        key_length: Some(64),
        value_length: Some(64),
        sliding_window: Some(128),
        sliding_window_layers: None,
        tensors,
    }
}

/// `blocks` layers with three 1000-byte experts each, 125 bytes of non-expert tensors per
/// block, and no extra tensors.
fn small_model(blocks: u32) -> GgufSummary {
    let mut tensors = Vec::new();
    for layer in 0..blocks {
        for suffix in [
            "ffn_up_exps.weight",
            "ffn_gate_exps.weight",
            "ffn_down_exps.weight",
        ] {
            tensors.push(GgufTensor {
                name: format!("blk.{layer}.{suffix}"),
                bytes: 1000,
            });
        }
        tensors.push(GgufTensor {
            name: format!("blk.{layer}.attn_q.weight"),
            bytes: 125,
        });
    }
    GgufSummary {
        architecture: "gpt-oss".into(),
        block_count: blocks,
        expert_count: Some(8),
        head_count_kv: None,
        key_length: None,
        value_length: None,
        sliding_window: None,
        sliding_window_layers: None,
        tensors,
    }
}

#[test]
fn parse_list_devices_reads_build_12014_output() {
    let stdout = "\
ggml_vulkan: Found 1 Vulkan devices:
Available devices:
  Vulkan0: AMD Radeon RX 5500 XT (RADV NAVI14) (8176 MiB, 8150 MiB free)
  RPC0: 127.0.0.1:50199 (30702 MiB, 30702 MiB free)
  CPU: Intel(R) Core(TM) i5-10400 CPU @ 2.90GHz (30702 MiB, 30702 MiB free)
transport: TCP
not a device line
";
    assert_eq!(
        parse_list_devices(stdout),
        vec![
            ListedDevice {
                name: "Vulkan0".into(),
                description: "AMD Radeon RX 5500 XT (RADV NAVI14)".into(),
                free_bytes: 8150 * MIB
            },
            ListedDevice {
                name: "RPC0".into(),
                description: "127.0.0.1:50199".into(),
                free_bytes: 30702 * MIB
            },
            ListedDevice {
                name: "CPU".into(),
                description: "Intel(R) Core(TM) i5-10400 CPU @ 2.90GHz".into(),
                free_bytes: 30702 * MIB
            },
        ]
    );
    assert!(parse_list_devices("garbage\n\n: nothing\n").is_empty());
}

#[test]
fn experts_fill_remote_devices_first_and_emit_exact_overrides() {
    let model = small_model(4);
    let budgets = Budgets {
        host_gpu: Some(("Vulkan0".into(), 10_000)),
        host_ram_bytes: 7_000,
        remote: vec![("RPC0".into(), 3_000), ("RPC1".into(), 3_000)],
        kv_bytes: 100,
    };
    let placement = plan(&model, &budgets).expect("plan");
    assert_eq!(
        placement,
        Placement {
            args: vec![
                "--device".into(),
                "Vulkan0,RPC0,RPC1".into(),
                "--tensor-split".into(),
                "1,0,0".into(),
                "-ngl".into(),
                "999".into(),
                "-ot".into(),
                r"blk\.(0)\.ffn_[a-z_]*_exps\.=RPC0".into(),
                "-ot".into(),
                r"blk\.(1)\.ffn_[a-z_]*_exps\.=RPC1".into(),
                "-ot".into(),
                r"blk\.(2|3)\.ffn_[a-z_]*_exps\.=CPU".into(),
                "-ot".into(),
                ".*=Vulkan0".into(),
            ],
            host_ram_bytes: 6_000,
            host_vram_bytes: 600,
            remote_bytes: 6_000,
            per_device: vec![
                ("Vulkan0".into(), 600),
                ("RPC0".into(), 3_000),
                ("RPC1".into(), 3_000),
                ("CPU".into(), 6_000),
            ],
        }
    );
    assert_eq!(
        plan(&model, &budgets).expect("plan again"),
        placement,
        "the planner is pure and deterministic"
    );
}

#[test]
fn a_gpt_oss_sized_model_uses_remote_capacity_before_host_ram() {
    let model = gpt_oss_120b();
    let non_expert = 36 * 100 * MIB + GIB;
    let budgets = Budgets {
        host_gpu: Some(("Vulkan0".into(), 2 * GIB + GIB / 2)),
        host_ram_bytes: 48 * GIB,
        remote: vec![("RPC0".into(), 100 * GIB), ("RPC1".into(), 100 * GIB)],
        kv_bytes: 0,
    };
    let placement = plan(&model, &budgets).expect("plan");

    assert_eq!(
        placement.host_vram_bytes, 0,
        "non-experts do not fit the 2.5 GiB host cap, so they go to RAM"
    );
    assert_eq!(
        placement.remote_bytes,
        32 * 6 * GIB,
        "16 layers on each RPC device (6 GiB per layer)"
    );
    assert_eq!(
        placement.host_ram_bytes,
        non_expert + 4 * 6 * GIB,
        "non-experts plus the four leftover layers"
    );
    assert!(placement.host_ram_bytes < 48 * GIB);
    assert_eq!(
        placement.per_device,
        vec![
            ("RPC0".into(), 16 * 6 * GIB),
            ("RPC1".into(), 16 * 6 * GIB),
            ("CPU".into(), non_expert + 24 * GIB),
        ]
    );
    assert_eq!(
        placement.args,
        vec![
            "--device".to_owned(),
            "RPC0,RPC1".to_owned(),
            "-ngl".to_owned(),
            "999".to_owned(),
            "--no-kv-offload".to_owned(),
            "-ot".to_owned(),
            r"blk\.(0|1|2|3|4|5|6|7|8|9|10|11|12|13|14|15)\.ffn_[a-z_]*_exps\.=RPC0".to_owned(),
            "-ot".to_owned(),
            r"blk\.(16|17|18|19|20|21|22|23|24|25|26|27|28|29|30|31)\.ffn_[a-z_]*_exps\.=RPC1"
                .to_owned(),
            "-ot".to_owned(),
            r"blk\.(32|33|34|35)\.ffn_[a-z_]*_exps\.=CPU".to_owned(),
            "-ot".to_owned(),
            ".*=CPU".to_owned(),
        ],
        "host GPU off: no tensor split, two RPC shares, KV stays on the CPU"
    );
    assert_eq!(
        plan(&model, &budgets).expect("plan again"),
        placement,
        "the planner is pure and deterministic"
    );
}

#[test]
fn everything_remote_leaves_cpu_out_of_the_device_list() {
    let model = small_model(4);
    let budgets = Budgets {
        host_gpu: Some(("Vulkan0".into(), 1_000)),
        host_ram_bytes: 0,
        remote: vec![("RPC0".into(), 12_000)],
        kv_bytes: 100,
    };
    let placement = plan(&model, &budgets).expect("plan");
    assert_eq!(placement.host_vram_bytes, 600);
    assert_eq!(placement.host_ram_bytes, 0);
    assert_eq!(placement.remote_bytes, 12_000);
    assert_eq!(
        placement.args,
        vec![
            "--device".to_owned(),
            "Vulkan0,RPC0".to_owned(),
            "--tensor-split".to_owned(),
            "1,0".to_owned(),
            "-ngl".to_owned(),
            "999".to_owned(),
            "-ot".to_owned(),
            r"blk\.(0|1|2|3)\.ffn_[a-z_]*_exps\.=RPC0".to_owned(),
            "-ot".to_owned(),
            ".*=Vulkan0".to_owned(),
        ],
        "one RPC device gets a zero share"
    );
    assert!(
        !placement.args.iter().any(|arg| arg.contains("=CPU")),
        "{:?}",
        placement.args
    );
    assert_eq!(
        placement.per_device,
        vec![("Vulkan0".into(), 600), ("RPC0".into(), 12_000)]
    );
}

#[test]
fn a_cpu_only_plan_omits_device_and_still_overrides_every_expert() {
    let model = small_model(4);
    let budgets = Budgets {
        host_gpu: None,
        host_ram_bytes: 20_000,
        remote: Vec::new(),
        kv_bytes: 0,
    };
    let placement = plan(&model, &budgets).expect("plan");
    assert_eq!(
        placement.args,
        vec![
            "-ngl".to_owned(),
            "999".to_owned(),
            "--no-kv-offload".to_owned(),
            "-ot".to_owned(),
            r"blk\.(0|1|2|3)\.ffn_[a-z_]*_exps\.=CPU".to_owned(),
            "-ot".to_owned(),
            ".*=CPU".to_owned(),
        ],
        "CPU is not a --device entry, and the host catch-all is last"
    );
    assert_eq!(placement.host_ram_bytes, 12_500);
    assert_eq!(placement.per_device, vec![("CPU".into(), 12_500)]);
}

#[test]
fn a_cpu_host_share_with_one_rpc_device_has_no_tensor_split() {
    let model = small_model(4);
    let budgets = Budgets {
        host_gpu: Some(("Vulkan0".into(), 0)),
        host_ram_bytes: 500,
        remote: vec![("RPC0".into(), 12_000)],
        kv_bytes: 0,
    };
    let placement = plan(&model, &budgets).expect("plan");
    assert_eq!(placement.host_vram_bytes, 0);
    assert_eq!(placement.host_ram_bytes, 500);
    assert_eq!(
        placement.args,
        vec![
            "--device".to_owned(),
            "RPC0".to_owned(),
            "-ngl".to_owned(),
            "999".to_owned(),
            "--no-kv-offload".to_owned(),
            "-ot".to_owned(),
            r"blk\.(0|1|2|3)\.ffn_[a-z_]*_exps\.=RPC0".to_owned(),
            "-ot".to_owned(),
            ".*=CPU".to_owned(),
        ],
        "the host GPU is off: no tensor split, and KV stays on the CPU"
    );
    assert_eq!(
        placement.per_device,
        vec![("RPC0".into(), 12_000), ("CPU".into(), 500)]
    );
}

#[test]
fn a_fused_expert_name_is_counted_and_overridden_by_the_same_rule() {
    let model = GgufSummary {
        architecture: "gpt-oss".into(),
        block_count: 3,
        expert_count: Some(64),
        head_count_kv: None,
        key_length: None,
        value_length: None,
        sliding_window: None,
        sliding_window_layers: None,
        tensors: vec![
            GgufTensor {
                name: "blk.0.ffn_gate_up_exps.weight".into(),
                bytes: 2_000,
            },
            GgufTensor {
                name: "blk.0.ffn_down_exps.weight".into(),
                bytes: 1_000,
            },
            GgufTensor {
                name: "blk.0.attn_q.weight".into(),
                bytes: 100,
            },
            GgufTensor {
                name: "blk.1.ffn_up_exps.weight".into(),
                bytes: 1_000,
            },
            GgufTensor {
                name: "blk.2.ffn_gate.weight".into(),
                bytes: 50,
            },
        ],
    };
    let budgets = Budgets {
        host_gpu: Some(("Vulkan0".into(), 1_000)),
        host_ram_bytes: 0,
        remote: vec![("RPC0".into(), 4_000)],
        kv_bytes: 100,
    };
    let placement = plan(&model, &budgets).expect("plan");
    assert_eq!(
        placement.args,
        vec![
            "--device".to_owned(),
            "Vulkan0,RPC0".to_owned(),
            "--tensor-split".to_owned(),
            "1,0".to_owned(),
            "-ngl".to_owned(),
            "999".to_owned(),
            "-ot".to_owned(),
            r"blk\.(0|1)\.ffn_[a-z_]*_exps\.=RPC0".to_owned(),
            "-ot".to_owned(),
            ".*=Vulkan0".to_owned(),
        ],
        "the fused expert layer is remote, not left to the host catch-all"
    );
    assert_eq!(placement.host_vram_bytes, 250, "non-experts plus KV");
    assert_eq!(placement.remote_bytes, 4_000, "both expert layers");
    assert_eq!(
        placement.per_device,
        vec![("Vulkan0".into(), 250), ("RPC0".into(), 4_000)],
        "no host RAM share and no CPU expert override"
    );
}

#[test]
fn leftovers_that_do_not_fit_are_reported_and_a_dense_model_has_no_experts() {
    let model = small_model(4);
    let budgets = Budgets {
        host_gpu: Some(("Vulkan0".into(), 10_000)),
        host_ram_bytes: 1_000,
        remote: Vec::new(),
        kv_bytes: 0,
    };
    assert_eq!(
        plan(&model, &budgets),
        Err(PlanError::DoesNotFit {
            needed: 3_000,
            available: 1_000
        })
    );

    let budgets = Budgets {
        host_gpu: Some(("Vulkan0".into(), 100)),
        host_ram_bytes: 100,
        remote: Vec::new(),
        kv_bytes: 0,
    };
    assert_eq!(
        plan(&model, &budgets),
        Err(PlanError::DoesNotFit {
            needed: 500,
            available: 100
        })
    );

    let dense = GgufSummary {
        architecture: "llama".into(),
        block_count: 32,
        expert_count: None,
        head_count_kv: None,
        key_length: None,
        value_length: None,
        sliding_window: None,
        sliding_window_layers: None,
        tensors: vec![GgufTensor {
            name: "token_embd.weight".into(),
            bytes: 1,
        }],
    };
    assert_eq!(plan(&dense, &budgets), Err(PlanError::NoExperts));
}

#[test]
fn kv_bytes_counts_gpt_oss_swa_layers_and_falls_back_when_metadata_is_missing() {
    let model = gpt_oss_120b();
    assert_eq!(
        kv_bytes(&model, 16_384),
        1_164_443_648,
        "18 full layers x 16384 + 18 SWA layers x 640 cells, at 2048 bytes per cell"
    );
    assert_eq!(kv_bytes(&model, 8_192), 862_453_760);
    assert_eq!(
        kv_bytes(&model, 128),
        546_308_096,
        "below the window the SWA layers hold only ctx cells"
    );

    let mut missing = model.clone();
    missing.head_count_kv = None;
    assert_eq!(
        kv_bytes(&missing, 16_384),
        1_744_830_464,
        "missing metadata falls back to full attention on every layer"
    );
    missing.head_count_kv = Some(8);
    missing.sliding_window = None;
    assert_eq!(kv_bytes(&missing, 16_384), 1_744_830_464);
}

#[test]
fn a_carried_sliding_window_pattern_overrides_the_gpt_oss_default() {
    let mut model = gpt_oss_120b();
    model.block_count = 4;
    model.head_count_kv = Some(2);
    model.key_length = Some(4);
    model.value_length = Some(4);
    model.sliding_window = Some(100);
    model.sliding_window_layers = Some(vec![false; 4]);
    assert_eq!(
        kv_bytes(&model, 1_000),
        128_000 + (512 << 20),
        "four full layers at 32 bytes per cell"
    );
    model.sliding_window_layers = Some(vec![true; 4]);
    assert_eq!(
        kv_bytes(&model, 1_000),
        78_336 + (512 << 20),
        "four SWA layers at min(1000, 612) cells"
    );
}

fn placement_with_host_share(host_ram_bytes: u64, host_vram_bytes: u64) -> Placement {
    Placement {
        args: Vec::new(),
        host_ram_bytes,
        host_vram_bytes,
        remote_bytes: 0,
        per_device: Vec::new(),
    }
}

#[test]
fn admission_honours_caps_live_memory_and_headroom() {
    let caps = HostCaps {
        ram_bytes: 6 * GIB,
        vram_bytes: 2 * GIB + GIB / 2,
    };
    let plan = placement_with_host_share(GIB, 500 * MIB);
    assert_eq!(admit(8 * GIB, GIB, &plan, &caps), Ok(()));
    assert_eq!(
        admit(GIB + HEADROOM_BYTES, GIB, &plan, &caps),
        Ok(()),
        "exactly the headroom is enough"
    );

    let over_ram = placement_with_host_share(7 * GIB, 0);
    assert_eq!(
        admit(16 * GIB, GIB, &over_ram, &caps),
        Err(AdmissionError::OverCap {
            what: "host RAM",
            wanted: 7 * GIB,
            cap: 6 * GIB
        })
    );

    let over_vram = placement_with_host_share(0, 3 * GIB);
    assert_eq!(
        admit(16 * GIB, 4 * GIB, &over_vram, &caps),
        Err(AdmissionError::OverCap {
            what: "host VRAM",
            wanted: 3 * GIB,
            cap: 2 * GIB + GIB / 2
        })
    );

    assert_eq!(
        admit(16 * GIB, 100 * MIB, &plan, &caps),
        Err(AdmissionError::OverCap {
            what: "free VRAM",
            wanted: 500 * MIB,
            cap: 100 * MIB
        })
    );

    assert_eq!(
        admit(GIB + HEADROOM_BYTES - 1, GIB, &plan, &caps),
        Err(AdmissionError::LowMemory {
            free: GIB + HEADROOM_BYTES - 1,
            wanted: GIB + HEADROOM_BYTES
        })
    );
    assert_eq!(HEADROOM_BYTES, 4 * GIB);
}
