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
        memory_pressure_avg10: None,
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
fn one_inference_is_admissible_when_its_threads_exceed_headroom_capacity() {
    // A small host: 4 logical CPUs, so the admission capacity is 4 - cpu_headroom(2) = 2.
    // llama-server still asks for one thread per CPU, which previously denied every inference
    // outright: nothing could ever run on a modest machine (or a 4-vCPU CI runner).
    let probe = Arc::new(Probe(Mutex::new(Some(HostSample {
        logical_cpus: 4,
        ..host()
    }))));
    let scheduler = Arc::new(ResourceScheduler::new(probe, AdmissionPolicy::default()));
    let mut claim = ResourceClaim::inference(WorkPriority::Voice);
    claim.cpu_slots = 4;
    let first = scheduler.try_admit(claim, Default::default()).unwrap();
    // Concurrency is still limited: a second CPU-heavy consumer is refused.
    assert!(
        scheduler
            .try_admit(
                ResourceClaim::inference(WorkPriority::Interactive),
                Default::default()
            )
            .is_err()
    );
    drop(first);
    assert!(
        scheduler
            .try_admit(
                ResourceClaim::inference(WorkPriority::Interactive),
                Default::default()
            )
            .is_ok()
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

#[tokio::test(start_paused = true)]
async fn voice_idle_waits_for_the_turn_and_a_quiet_period() {
    let (_, scheduler) = setup();
    let quiet = std::time::Duration::from_secs(2);
    let before = scheduler.voice_epoch();
    let turn = scheduler.voice_turn();
    assert!(scheduler.voice_active());
    assert_ne!(scheduler.voice_epoch(), before, "a turn advances the epoch");

    let waiter = {
        let scheduler = scheduler.clone();
        tokio::spawn(async move { scheduler.voice_idle(quiet).await })
    };
    tokio::time::sleep(std::time::Duration::from_secs(10)).await;
    assert!(!waiter.is_finished(), "returned while voice was active");

    drop(turn);
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    assert!(!waiter.is_finished(), "returned before the quiet period");
    // A follow-up question within the quiet period restarts the wait.
    let follow_up = scheduler.voice_turn();
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    assert!(!waiter.is_finished(), "returned during a follow-up turn");
    drop(follow_up);
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    assert!(waiter.is_finished(), "still waiting after voice went quiet");
}

#[tokio::test(start_paused = true)]
async fn voice_idle_returns_after_the_quiet_period_when_no_voice() {
    let (_, scheduler) = setup();
    let start = tokio::time::Instant::now();
    scheduler
        .voice_idle(std::time::Duration::from_millis(500))
        .await;
    assert!(start.elapsed() >= std::time::Duration::from_millis(500));
    assert!(!scheduler.voice_active());
}

#[test]
fn idle_swapped_pages_do_not_block_work_but_real_memory_pressure_does() {
    let (probe, scheduler) = setup();
    // Swap in use but no stall on memory: pages swapped out long ago and left there.
    *probe.0.lock().unwrap() = Some(HostSample {
        swap_used_bytes: 3 * GIB,
        memory_pressure_avg10: Some(0.0),
        ..host()
    });
    let lease = scheduler.try_admit(ResourceClaim::build(), Default::default());
    assert!(lease.is_ok(), "{:?}", lease.err());
    drop(lease);

    // Tasks stalling on memory: refused, whatever the swap reading, voice included.
    *probe.0.lock().unwrap() = Some(HostSample {
        swap_used_bytes: 0,
        memory_pressure_avg10: Some(25.0),
        ..host()
    });
    let Err(error) = scheduler.try_admit(
        ResourceClaim::inference(WorkPriority::Voice),
        Default::default(),
    ) else {
        panic!("admitted under memory pressure");
    };
    assert!(error.to_string().contains("memory pressure"), "{error}");

    // Kernel without PSI: the swap-used limit still applies.
    *probe.0.lock().unwrap() = Some(HostSample {
        swap_used_bytes: 3 * GIB,
        memory_pressure_avg10: None,
        ..host()
    });
    assert!(
        scheduler
            .try_admit(ResourceClaim::build(), Default::default())
            .is_err()
    );
}

#[test]
fn memory_pressure_is_parsed_from_proc_pressure_memory() {
    let text = "some avg10=12.50 avg60=3.10 avg300=0.40 total=149591882\nfull avg10=1.00 avg60=0.00 avg300=0.00 total=146955704\n";
    assert_eq!(parse_memory_pressure(text), Some(12.5));
    assert_eq!(parse_memory_pressure("full avg10=1.00\n"), None);
    assert_eq!(parse_memory_pressure("some avg10=nan\n"), None);
    assert_eq!(parse_memory_pressure("some avg10=250\n"), None);
    assert_eq!(parse_memory_pressure(""), None);
}

#[test]
fn real_linux_host_reports_memory_pressure_when_the_kernel_has_psi() {
    let sample = ProcHostProbe.sample().unwrap();
    if std::path::Path::new("/proc/pressure/memory").exists() {
        let pressure = sample
            .memory_pressure_avg10
            .expect("PSI present but not read");
        assert!((0.0..=100.0).contains(&pressure));
    } else {
        assert!(sample.memory_pressure_avg10.is_none());
    }
}
