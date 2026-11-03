use sail_common::resources::*;
use sail_protocol::resources::*;
use std::{
    io,
    sync::{Arc, Barrier, Mutex},
};
use tokio_util::sync::CancellationToken;

struct Probe(Mutex<Option<HostSample>>);
impl HostProbe for Probe {
    fn sample(&self) -> io::Result<HostSample> {
        self.0
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| io::Error::other("probe offline"))
    }
}
fn host() -> HostSample {
    HostSample {
        available_ram_bytes: 24 * GIB,
        total_ram_bytes: 32 * GIB,
        swap_used_bytes: 0,
        swap_total_bytes: 8 * GIB,
        logical_cpus: 12,
    }
}
fn setup() -> (Arc<Probe>, Arc<ResourceScheduler>) {
    let probe = Arc::new(Probe(Mutex::new(Some(host()))));
    let scheduler = Arc::new(ResourceScheduler::new(
        probe.clone(),
        AdmissionPolicy::default(),
    ));
    (probe, scheduler)
}
fn model() -> ResourceClaim {
    ResourceClaim {
        kind: WorkKind::Model,
        priority: WorkPriority::Interactive,
        ram_bytes: 20 * GIB,
        cpu_slots: 0,
        heavy_model: true,
    }
}
#[test]
fn parser_requires_real_available_memory_swap_and_units() {
    let valid = "MemTotal: 100 kB\nMemAvailable: 40 kB\nSwapTotal: 10 kB\nSwapFree: 6 kB\n";
    let sample = parse_meminfo(valid, 4).unwrap();
    assert_eq!(sample.available_ram_bytes, 40 * 1024);
    assert_eq!(sample.swap_used_bytes, 4 * 1024);
    for invalid in [
        valid.replace("MemAvailable", "MemFree"),
        valid.replace("40 kB", "101 kB"),
        valid.replace("6 kB", "11 kB"),
        valid.replace("kB", "MB"),
        valid.replace("100", "bad"),
    ] {
        assert!(parse_meminfo(&invalid, 4).is_err());
    }
}
#[test]
fn memory_swap_and_unknown_measurements_fail_closed_for_voice_too() {
    let (probe, scheduler) = setup();
    probe
        .0
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .available_ram_bytes = 2 * GIB;
    assert!(
        scheduler
            .try_admit(
                ResourceClaim::inference(WorkPriority::Voice),
                Default::default()
            )
            .is_err()
    );
    *probe.0.lock().unwrap() = Some(HostSample {
        swap_used_bytes: 2 * GIB,
        ..host()
    });
    assert!(
        scheduler
            .try_admit(ResourceClaim::build(), Default::default())
            .is_err()
    );
    *probe.0.lock().unwrap() = None;
    assert!(
        scheduler
            .try_admit(ResourceClaim::build(), Default::default())
            .is_err()
    );
    let status = scheduler.status();
    assert!(status.observed.is_none());
    assert!(status.observation_error.unwrap().contains("offline"));
    assert_eq!(status.rejected, 3);
}
#[test]
fn startup_reservations_are_atomic_settle_into_observed_memory_and_keep_residency() {
    let (probe, scheduler) = setup();
    let lease = scheduler.try_admit(model(), Default::default()).unwrap();
    assert_eq!(scheduler.status().pending_ram_bytes, 20 * GIB);
    assert!(scheduler.try_admit(model(), Default::default()).is_err());
    probe
        .0
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .available_ram_bytes = 4 * GIB;
    lease.settled();
    assert_eq!(scheduler.status().pending_ram_bytes, 0);
    assert_eq!(scheduler.status().resident_heavy_models, 1);
    let inference = scheduler
        .try_admit(
            ResourceClaim::inference(WorkPriority::Voice),
            Default::default(),
        )
        .unwrap();
    // The residency limit remains after settling; unloading frees it.
    assert!(scheduler.try_admit(model(), Default::default()).is_err());
    drop(inference);
    drop(lease);
    *probe.0.lock().unwrap() = Some(host());
    assert!(scheduler.try_admit(model(), Default::default()).is_ok());
}
#[test]
fn voice_preempts_background_inference_and_builds_and_blocks_new_background_work() {
    let (_, scheduler) = setup();
    let cancel = CancellationToken::new();
    let _background = scheduler
        .try_admit(
            ResourceClaim::inference(WorkPriority::Background),
            cancel.clone(),
        )
        .unwrap();
    let build_cancel = CancellationToken::new();
    let build = scheduler
        .try_admit(ResourceClaim::build(), build_cancel.clone())
        .unwrap();
    let interactive_cancel = CancellationToken::new();
    let _interactive = scheduler
        .try_admit(
            ResourceClaim::inference(WorkPriority::Interactive),
            interactive_cancel.clone(),
        )
        .unwrap();
    let voice = scheduler.voice_turn();
    let nested = scheduler.voice_turn();
    assert!(cancel.is_cancelled());
    assert!(build_cancel.is_cancelled());
    assert!(!interactive_cancel.is_cancelled());
    assert_eq!(scheduler.status().preemptions, 2);
    assert!(
        scheduler
            .try_admit(ResourceClaim::browser(GIB), Default::default())
            .is_err()
    );
    drop(voice);
    assert_eq!(scheduler.status().voice_turns, 1);
    drop(nested);
    drop(build);
    assert!(
        scheduler
            .try_admit(ResourceClaim::build(), Default::default())
            .is_ok()
    );
}
#[test]
fn cpu_slots_browser_and_build_concurrency_release_on_drop() {
    let (_, scheduler) = setup();
    let mut claim = ResourceClaim::inference(WorkPriority::Background);
    claim.cpu_slots = 10;
    let cpu = scheduler.try_admit(claim, Default::default()).unwrap();
    assert!(
        scheduler
            .try_admit(ResourceClaim::build(), Default::default())
            .is_err()
    );
    drop(cpu);
    let browser = scheduler
        .try_admit(ResourceClaim::browser(GIB), Default::default())
        .unwrap();
    assert!(
        scheduler
            .try_admit(ResourceClaim::browser(GIB), Default::default())
            .is_err()
    );
    let build = scheduler
        .try_admit(ResourceClaim::build(), Default::default())
        .unwrap();
    assert!(
        scheduler
            .try_admit(ResourceClaim::build(), Default::default())
            .is_err()
    );
    drop((build, browser));
    assert_eq!(scheduler.status().reserved_cpu_slots, 0);
}
#[test]
fn concurrent_launchers_cannot_both_take_the_browser_slot() {
    let (_, scheduler) = setup();
    let barrier = Arc::new(Barrier::new(2));
    let handles: Vec<_> = (0..2)
        .map(|_| {
            let scheduler = scheduler.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                let lease = scheduler.try_admit(ResourceClaim::browser(GIB), Default::default());
                barrier.wait(); // Hold the successful lease until both have attempted admission.
                lease.is_ok()
            })
        })
        .collect();
    assert_eq!(
        handles
            .into_iter()
            .map(|h| usize::from(h.join().unwrap()))
            .sum::<usize>(),
        1
    );
}
#[test]
fn real_linux_probe_reports_observations_without_allocating_workloads() {
    let observed = ProcHostProbe.sample().unwrap();
    assert!(observed.total_ram_bytes > 0);
    assert!(observed.available_ram_bytes <= observed.total_ram_bytes);
    assert!(observed.swap_used_bytes <= observed.swap_total_bytes);
}

#[tokio::test]
async fn raw_inference_wrapper_is_not_polled_when_denied_and_drops_on_voice_preemption() {
    let (probe, scheduler) = setup();
    *probe.0.lock().unwrap() = None;
    assert!(
        scheduler
            .background_inference(async { panic!("unadmitted operation polled") })
            .await
            .is_err()
    );
    *probe.0.lock().unwrap() = Some(host());
    let notify = Arc::new(tokio::sync::Notify::new());
    let started = notify.clone();
    let task = tokio::spawn({
        let scheduler = scheduler.clone();
        async move {
            scheduler
                .background_inference(async move {
                    started.notify_one();
                    std::future::pending::<()>().await
                })
                .await
        }
    });
    notify.notified().await;
    let _voice = scheduler.voice_turn();
    assert!(
        tokio::time::timeout(std::time::Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap()
            .is_err()
    );
    assert_eq!(scheduler.status().admitted_inference, 0);
}

#[test]
fn background_speaker_worker_is_preempted_but_live_speech_is_preserved() {
    let (_, scheduler) = setup();
    let background = CancellationToken::new();
    let live = CancellationToken::new();
    let _analysis = scheduler
        .try_admit(
            ResourceClaim {
                kind: WorkKind::Speech,
                priority: WorkPriority::Background,
                ram_bytes: 128 * MIB,
                cpu_slots: 1,
                heavy_model: false,
            },
            background.clone(),
        )
        .unwrap();
    let _speech = scheduler
        .try_admit(
            ResourceClaim {
                kind: WorkKind::Speech,
                priority: WorkPriority::Voice,
                ram_bytes: 128 * MIB,
                cpu_slots: 0,
                heavy_model: false,
            },
            live.clone(),
        )
        .unwrap();
    let _voice = scheduler.voice_turn();
    assert!(background.is_cancelled());
    assert!(!live.is_cancelled());
    assert!(
        scheduler
            .try_admit(
                ResourceClaim {
                    kind: WorkKind::Speech,
                    priority: WorkPriority::Background,
                    ram_bytes: 128 * MIB,
                    cpu_slots: 1,
                    heavy_model: false
                },
                Default::default()
            )
            .is_err()
    );
}
