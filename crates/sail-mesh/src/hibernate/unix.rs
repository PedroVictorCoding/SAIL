use std::io;

use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, Signal, System};

use super::{ProcInfo, ProcessControl};

pub struct UnixProcessControl {
    sys: Option<System>,
}

impl UnixProcessControl {
    pub fn new() -> Self {
        Self {
            sys: System::new().ok(),
        }
    }
}

impl ProcessControl for UnixProcessControl {
    fn list(&mut self) -> Vec<ProcInfo> {
        let Some(sys) = self.sys.as_mut() else {
            return Vec::new();
        };
        sys.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::nothing(),
        );
        sys.processes()
            .iter()
            .map(|(pid, proc_)| ProcInfo {
                pid: pid.as_u32(),
                parent: proc_.parent().map(|p| p.as_u32()),
                name: proc_.name().to_string_lossy().into_owned(),
                start_time: proc_.start_time(),
                working_set: proc_.memory(),
                full_screen: false,
                foreground: false,
            })
            .collect()
    }

    fn suspend(&mut self, pid: u32) -> io::Result<()> {
        let sys = self
            .sys
            .as_mut()
            .ok_or_else(|| io::Error::other("process monitor unavailable"))?;
        sys.refresh_processes_specifics(
            ProcessesToUpdate::Some(&[Pid::from_u32(pid)]),
            true,
            ProcessRefreshKind::nothing(),
        );
        let proc_ = sys
            .process(Pid::from_u32(pid))
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "process not found"))?;
        if proc_.kill_with(Signal::Stop) == Some(true) {
            Ok(())
        } else {
            Err(io::Error::other("failed to send SIGSTOP"))
        }
    }

    fn resume(&mut self, pid: u32) -> io::Result<()> {
        let sys = self
            .sys
            .as_mut()
            .ok_or_else(|| io::Error::other("process monitor unavailable"))?;
        sys.refresh_processes_specifics(
            ProcessesToUpdate::Some(&[Pid::from_u32(pid)]),
            true,
            ProcessRefreshKind::nothing(),
        );
        let proc_ = sys
            .process(Pid::from_u32(pid))
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "process not found"))?;
        if proc_.kill_with(Signal::Continue) == Some(true) {
            Ok(())
        } else {
            Err(io::Error::other("failed to send SIGCONT"))
        }
    }

    fn trim(&mut self, _pid: u32) -> io::Result<u64> {
        Ok(0)
    }

    fn free_commit(&mut self) -> Option<u64> {
        let sys = self.sys.as_mut()?;
        sys.refresh_memory();
        Some(sys.available_memory().saturating_add(sys.free_swap()))
    }
}
