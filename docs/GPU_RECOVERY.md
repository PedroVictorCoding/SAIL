# RX 5500 Vulkan recovery

## Observed failure, 2026-11-05

The AMD kernel driver reported a graphics-ring timeout for llama-server followed by
`GPU Recovery Failed: -5`. A later `llama-server --list-devices` process was stuck
in uninterruptible sleep (`D`, `amdgpu_info_ioctl`). The model health endpoint on
127.0.0.1:8081 was unavailable; a Dialogue inference attempt hit its 180-second
deadline. This is a failed GPU/driver state, not proof of a microphone fault.

The host used kernel 7.0.0-42-generic, Mesa RADV 26.0.8, RX 5500/Navi14, and
llama.cpp aa588a346. The requested profile used hybrid offload, 99 GPU layers,
Flash Attention, q4_0 KV caches, and 512-token microbatches. The precise trigger
is not established. Do not attribute it to VRAM exhaustion or Flash Attention
without reproducing it on a recovered GPU.

SAIL previously omitted `-fa` when Flash Attention was disabled, but this
llama.cpp version defaults to `auto`. SAIL now sends `-fa off` explicitly.
This fixes the recovery control, not the AMD driver itself.

## Recovery and verification

1. Save work and obtain the owner's approval before rebooting. A failed driver
   reset and an uninterruptible ioctl prevent meaningful GPU validation now.
   Do not launch repeated enumeration probes, force a live GPU reset, or change
   power/clock settings while the desktop uses the GPU.
2. After reboot, check `journalctl -k -b` for fresh amdgpu faults before testing.
   Use the task branch containing the explicit Flash Attention fix.
3. Start a conservative **unverified diagnostic profile**, keeping the same model
   and expert weights on CPU while testing a few GPU layers:

   ```bash
   SAIL_CONTEXT_TOKENS=4096 SAIL_BATCH_SIZE=128 SAIL_UBATCH_SIZE=64 \
     SAIL_CACHE_REUSE=0 ./run.sh --restart --hybrid --gpu-layers 4 \
     --cpu-moe all --no-flash-attn --cache-type-k f16 --cache-type-v f16
   ```

   These settings reduce simultaneous work and avoid quantized KV/automatic
   Flash Attention. They are a diagnostic starting point, not a measured optimum
   or a proven workaround. No saved acceleration profile is overwritten.
4. Verify `/api/mind/status` reports ready and the observed backend uses Vulkan;
   verify http://127.0.0.1:8081/health. Test short text, longer prefill, repeated
   turns, cancellation, and an actual microphone-to-spoken-answer interaction.
   Check kernel and model logs after each test. Record first-token latency,
   generation throughput and VRAM usage; a passing health check alone is insufficient.
5. If stable, increase offload in small steps and measure against CPU on the same
   prompts. Change one parameter at a time. Enable automatic startup only after
   sustained real voice tests pass and GPU measurements justify it. The existing
   `mind-gpu-setup.sh` forces Flash Attention in its benchmark, so it must not be
   used to certify this Flash Attention-disabled diagnostic profile.
6. If the GPU faults again, stop testing and preserve the new kernel/model logs.
   Compare a supported updated kernel/Mesa/firmware or a separate llama.cpp build
   only as a controlled follow-up; do not upgrade drivers or reboot implicitly.

Related upstream reports describe device loss on RX 5500 in whisper.cpp
([#3611](https://github.com/ggml-org/whisper.cpp/issues/3611)) and Flash Attention
device loss on a different AMD GPU in llama.cpp
([#20889](https://github.com/ggml-org/llama.cpp/issues/20889)). These are leads,
not confirmation of the cause on this host.
