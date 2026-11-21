use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use sail_mesh::hibernate::{
    Hibernator, JournalEntry, MIN_FREE_COMMIT_HEADROOM_BYTES, ProcInfo, ProcessControl, eligible,
    recovery_plan,
};
use tempfile::TempDir;

#[derive(Default)]
struct FakeState {
    procs: Vec<ProcInfo>,
    suspended: Vec<u32>,
    resumed: Vec<u32>,
    trimmed: Vec<u32>,
    free_commit: Option<u64>,
    journal_path: Option<PathBuf>,
    saw_journal_before_suspend: bool,
}

struct FakeControl {
    state: Arc<Mutex<FakeState>>,
}

impl FakeControl {
    fn new(state: Arc<Mutex<FakeState>>) -> Self {
        Self { state }
    }
}

impl ProcessControl for FakeControl {
    fn list(&mut self) -> Vec<ProcInfo> {
        self.state.lock().unwrap().procs.clone()
    }

    fn suspend(&mut self, pid: u32) -> io::Result<()> {
        let mut st = self.state.lock().unwrap();
        if let Some(ref jpath) = st.journal_path
            && let Ok(bytes) = std::fs::read(jpath)
            && let Ok(entries) = serde_json::from_slice::<Vec<JournalEntry>>(&bytes)
            && entries.iter().any(|e| e.pid == pid)
        {
            st.saw_journal_before_suspend = true;
        }
        st.suspended.push(pid);
        Ok(())
    }

    fn resume(&mut self, pid: u32) -> io::Result<()> {
        self.state.lock().unwrap().resumed.push(pid);
        Ok(())
    }

    fn trim(&mut self, pid: u32) -> io::Result<u64> {
        let mut st = self.state.lock().unwrap();
        st.trimmed.push(pid);
        Ok(256 * 1024 * 1024)
    }

    fn free_commit(&mut self) -> Option<u64> {
        self.state.lock().unwrap().free_commit
    }
}

fn proc_item(pid: u32, name: &str, start_time: u64, ws: u64) -> ProcInfo {
    ProcInfo {
        pid,
        parent: Some(100),
        name: name.to_string(),
        start_time,
        working_set: ws,
        full_screen: false,
        foreground: false,
    }
}

#[test]
fn journal_is_written_before_suspend_and_resume_all_is_idempotent() {
    let dir = TempDir::new().unwrap();
    let state = Arc::new(Mutex::new(FakeState {
        procs: vec![proc_item(420, "chrome.exe", 9001, 512 * 1024 * 1024)],
        free_commit: Some(8 * 1024 * 1024 * 1024),
        journal_path: Some(dir.path().join("hibernate.json")),
        ..Default::default()
    }));

    let mut hib = Hibernator::new(Box::new(FakeControl::new(state.clone())), dir.path());
    hib.add(&["chrome.exe".into()]).unwrap();

    let apps = hib.hibernate(&[100]).unwrap();
    assert_eq!(apps.len(), 1);
    assert_eq!(apps[0].name, "chrome.exe");
    assert_eq!(apps[0].processes, 1);
    assert_eq!(apps[0].released_bytes, 256 * 1024 * 1024);

    {
        let st = state.lock().unwrap();
        assert!(st.saw_journal_before_suspend);
        assert_eq!(st.suspended, vec![420]);
        assert_eq!(st.trimmed, vec![420]);
    }

    assert_eq!(hib.resume_all().unwrap(), 1);
    assert_eq!(hib.resume_all().unwrap(), 0);
    assert!(hib.hibernated().is_empty());
    assert_eq!(state.lock().unwrap().resumed, vec![420]);
}

#[test]
fn crash_recovery_resumes_matching_and_skips_reused_pid() {
    let dir = TempDir::new().unwrap();
    let entries = vec![
        JournalEntry {
            pid: 500,
            name: "Code.exe".into(),
            start_time: 1111,
            since: "2026-11-21T01:00:00Z".into(),
            released_bytes: 100,
        },
        JournalEntry {
            pid: 600,
            name: "chrome.exe".into(),
            start_time: 2222,
            since: "2026-11-21T01:00:00Z".into(),
            released_bytes: 100,
        },
    ];
    std::fs::write(
        dir.path().join("hibernate.json"),
        serde_json::to_vec(&entries).unwrap(),
    )
    .unwrap();

    let state = Arc::new(Mutex::new(FakeState {
        procs: vec![
            proc_item(500, "Code.exe", 1111, 100),
            proc_item(600, "notepad.exe", 9999, 100), // reused PID!
        ],
        ..Default::default()
    }));

    let mut hib = Hibernator::new(Box::new(FakeControl::new(state.clone())), dir.path());
    let resumed = hib.recover().unwrap();
    assert_eq!(resumed, 1);
    assert_eq!(state.lock().unwrap().resumed, vec![500]);
    assert!(hib.hibernated().is_empty());
}

#[test]
fn never_list_and_pure_determinism_are_honoured() {
    let mut fullscreen = proc_item(703, "chrome.exe", 4, 100);
    fullscreen.full_screen = true;

    let procs = vec![
        proc_item(1, "systemd", 1, 100),
        proc_item(10, "svchost.exe", 2, 100),
        proc_item(11, "dwm.exe", 3, 100),
        proc_item(12, "explorer.exe", 4, 100),
        proc_item(20, "reaper.exe", 5, 100),
        proc_item(21, "ableton live.exe", 6, 100),
        proc_item(30, "sail-mesh-worker.exe", 7, 100),
        proc_item(31, "chrome.exe", 8, 100),   // own descendant
        proc_item(701, "notepad.exe", 9, 100), // not listed
        fullscreen,
        proc_item(704, "Code.exe", 10, 100),
    ];
    let listed = vec![
        "chrome.exe".into(),
        "Code.exe".into(),
        "svchost.exe".into(),
        "reaper.exe".into(),
    ];
    let own = vec![30, 31];

    let first = eligible(&procs, &listed, &own);
    let second = eligible(&procs, &listed, &own);
    assert_eq!(first, second);
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].pid, 704);

    let j = vec![JournalEntry {
        pid: 704,
        name: "Code.exe".into(),
        start_time: 10,
        since: "2026-11-21T01:00:00Z".into(),
        released_bytes: 0,
    }];
    assert_eq!(recovery_plan(&j, &procs), recovery_plan(&j, &procs));
}

#[test]
fn focus_change_reports_owner_returned() {
    let dir = TempDir::new().unwrap();
    let state = Arc::new(Mutex::new(FakeState {
        procs: vec![proc_item(800, "chrome.exe", 55, 100)],
        free_commit: Some(8 * 1024 * 1024 * 1024),
        ..Default::default()
    }));

    let mut hib = Hibernator::new(Box::new(FakeControl::new(state.clone())), dir.path());
    hib.add(&["chrome.exe".into()]).unwrap();
    hib.hibernate(&[]).unwrap();
    assert!(!hib.owner_returned());

    state.lock().unwrap().procs[0].foreground = true;
    assert!(hib.owner_returned());
}

#[test]
fn low_free_commit_refuses_to_hibernate() {
    let dir = TempDir::new().unwrap();
    let ws = 1024 * 1024 * 1024;
    let state = Arc::new(Mutex::new(FakeState {
        procs: vec![proc_item(900, "chrome.exe", 77, ws)],
        free_commit: Some(MIN_FREE_COMMIT_HEADROOM_BYTES + ws - 1),
        ..Default::default()
    }));

    let mut hib = Hibernator::new(Box::new(FakeControl::new(state.clone())), dir.path());
    hib.add(&["chrome.exe".into()]).unwrap();
    let err = hib.hibernate(&[]).unwrap_err();
    assert!(err.to_string().contains("not enough free commit"));
    assert!(state.lock().unwrap().suspended.is_empty());
}

#[test]
fn malformed_journal_never_panics() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("hibernate.json"), b"{not valid json").unwrap();
    let state = Arc::new(Mutex::new(FakeState::default()));
    let mut hib = Hibernator::new(Box::new(FakeControl::new(state)), dir.path());
    assert_eq!(hib.recover().unwrap(), 0);
    assert!(hib.hibernated().is_empty());
}
