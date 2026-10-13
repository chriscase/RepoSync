//! Unix fixture-only process ownership. Each service starts its own group;
//! cleanup signals that group, then reaps the direct child. No process-global
//! environment changes or platform-specific subreaper are required.

use std::ops::{Deref, DerefMut};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

pub struct OwnedProcessGroup(Child);

impl OwnedProcessGroup {
    pub fn spawn(command: &mut Command) -> std::io::Result<Self> {
        command.process_group(0).spawn().map(Self)
    }

    fn signal_group(&self, signal: rustix::process::Signal) {
        // A typed positive PID names only the group created for our own child;
        // use the portable syscall wrapper rather than parsing kill CLI flags.
        let group = i32::try_from(self.0.id())
            .ok()
            .and_then(rustix::process::Pid::from_raw);
        if let Some(group) = group {
            let _ = rustix::process::kill_process_group(group, signal);
        }
    }
}

impl Deref for OwnedProcessGroup {
    type Target = Child;
    fn deref(&self) -> &Child {
        &self.0
    }
}

impl DerefMut for OwnedProcessGroup {
    fn deref_mut(&mut self) -> &mut Child {
        &mut self.0
    }
}

impl Drop for OwnedProcessGroup {
    fn drop(&mut self) {
        self.signal_group(rustix::process::Signal::TERM);
        let deadline = Instant::now() + Duration::from_millis(500);
        while self.0.try_wait().ok().flatten().is_none() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        // Include descendants that ignored TERM even if the leader exited.
        self.signal_group(rustix::process::Signal::KILL);
        let _ = self.0.wait();
    }
}
