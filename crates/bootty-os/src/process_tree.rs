//! Child processes whose descendants end with them: a process group on Unix, a kill-on-close
//! job object on Windows.

use std::io;
use std::process::{Child, Command, ExitStatus};
use std::sync::Arc;

/// A child whose whole process tree is isolated from the caller.
///
/// Dropping a live value terminates and reaps the tree. A successful [`Self::wait`] disarms that
/// cleanup, so descendants a finished child left in the background keep running, as they would
/// under `std::process::Child`.
pub struct ProcessTreeChild {
    child: Child,
    tree: Arc<Tree>,
    armed: bool,
}

/// Whether a spawned tree may create a console window. Only Windows has the distinction.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ConsoleWindowBehavior {
    #[default]
    Inherit,
    /// Background helpers never flash a console window.
    Suppress,
}

/// A clonable handle that terminates the tree while another owner holds the child.
#[derive(Clone)]
pub struct ProcessTreeController {
    tree: Arc<Tree>,
}

impl ProcessTreeController {
    /// Force-terminates every process still in the tree. A reaped tree is a no-op.
    ///
    /// # Errors
    /// Returns the platform's termination failure.
    pub fn terminate(&self) -> io::Result<()> {
        self.tree.terminate()
    }
}

impl ProcessTreeChild {
    /// Spawns `command` in a new tree, isolated before it can start another process.
    ///
    /// # Errors
    /// Returns the spawn or isolation failure; a child that could not be isolated is killed.
    pub fn spawn(command: &mut Command) -> io::Result<Self> {
        Self::spawn_with_console_window(command, ConsoleWindowBehavior::Inherit)
    }

    /// [`Self::spawn`] with an explicit Windows console-window policy.
    ///
    /// # Errors
    /// Returns the spawn or isolation failure; a child that could not be isolated is killed.
    pub fn spawn_with_console_window(
        command: &mut Command,
        console_window: ConsoleWindowBehavior,
    ) -> io::Result<Self> {
        let (child, tree) = Tree::spawn(command, console_window)?;
        Ok(Self {
            child,
            tree: Arc::new(tree),
            armed: true,
        })
    }

    #[must_use]
    pub fn controller(&self) -> ProcessTreeController {
        ProcessTreeController {
            tree: Arc::clone(&self.tree),
        }
    }

    /// The direct child, for taking its configured standard I/O.
    pub const fn child_mut(&mut self) -> &mut Child {
        &mut self.child
    }

    /// Whether the direct child has exited, without reaping it.
    ///
    /// # Errors
    /// Returns the status query failure.
    pub fn has_exited(&mut self) -> io::Result<bool> {
        if !self.armed {
            return Ok(true);
        }
        self.tree.has_exited(&mut self.child)
    }

    /// Force-terminates the direct child and every descendant.
    ///
    /// # Errors
    /// Returns the platform's termination failure.
    pub fn terminate(&mut self) -> io::Result<()> {
        if !self.armed {
            return Ok(());
        }
        self.tree.terminate()
    }

    /// Waits for the direct child and disarms tree cleanup.
    ///
    /// # Errors
    /// Returns the wait failure.
    pub fn wait(&mut self) -> io::Result<ExitStatus> {
        let status = self.tree.wait(&mut self.child)?;
        self.armed = false;
        Ok(status)
    }
}

impl Drop for ProcessTreeChild {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let _ = self.terminate();
        let _ = self.child.kill();
        if self.wait().is_err() {
            self.tree.disarm();
        }
    }
}

#[cfg(windows)]
use crate::windows::ProcessJob as Tree;

#[cfg(unix)]
use unix::ProcessGroup as Tree;

#[cfg(unix)]
mod unix {
    use crate::spawn::CommandSpawn as _;
    use std::io;
    use std::os::unix::process::CommandExt as _;
    use std::process::{Child, Command, ExitStatus};
    use std::sync::{Mutex, MutexGuard, PoisonError};

    use rustix::io::Errno;
    use rustix::process::{Pid, Signal, WaitId, WaitIdOptions, kill_process_group, waitid};

    use super::ConsoleWindowBehavior;

    /// A process group whose numeric ID is signalled only while its leader is unreaped, so a
    /// recycled ID is never hit.
    pub(super) struct ProcessGroup {
        id: Pid,
        active: Mutex<bool>,
    }

    impl ProcessGroup {
        pub(super) fn spawn(
            command: &mut Command,
            _console_window: ConsoleWindowBehavior,
        ) -> io::Result<(Child, Self)> {
            command.process_group(0);
            let child = command.bootty_spawn()?;
            let id = Pid::from_child(&child);
            Ok((
                child,
                Self {
                    id,
                    active: Mutex::new(true),
                },
            ))
        }

        pub(super) fn terminate(&self) -> io::Result<()> {
            let active = self.lock_active();
            let result = if *active {
                match kill_process_group(self.id, Signal::KILL) {
                    Ok(()) | Err(Errno::SRCH) => Ok(()),
                    // macOS refuses to signal a group whose members are all gone but whose
                    // leader is still a zombie.
                    Err(Errno::PERM) => self.leader_exited().and_then(|exited| {
                        if exited {
                            Ok(())
                        } else {
                            Err(Errno::PERM.into())
                        }
                    }),
                    Err(error) => Err(error.into()),
                }
            } else {
                Ok(())
            };
            drop(active);
            result
        }

        pub(super) fn has_exited(&self, _child: &mut Child) -> io::Result<bool> {
            self.leader_exited()
        }

        fn leader_exited(&self) -> io::Result<bool> {
            Ok(waitid(
                WaitId::Pid(self.id),
                WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT,
            )?
            .is_some_and(|status| status.exited() || status.killed() || status.dumped()))
        }

        /// Waits without reaping, then reaps under the same lock `terminate` takes, closing the
        /// reap versus signal race.
        pub(super) fn wait(&self, child: &mut Child) -> io::Result<ExitStatus> {
            if let Some(status) = self.reap(Child::try_wait, child)? {
                return Ok(status);
            }
            loop {
                match waitid(
                    WaitId::Pid(self.id),
                    WaitIdOptions::EXITED | WaitIdOptions::NOWAIT,
                ) {
                    Err(Errno::INTR) => {}
                    result => {
                        result?;
                        break;
                    }
                }
            }
            self.reap(|child| child.wait().map(Some), child)?
                .ok_or_else(|| io::Error::other("the process group leader was not reaped"))
        }

        /// Reaps under the lock, disarming the group once its leader is gone.
        fn reap(
            &self,
            reap: impl FnOnce(&mut Child) -> io::Result<Option<ExitStatus>>,
            child: &mut Child,
        ) -> io::Result<Option<ExitStatus>> {
            let mut active = self.lock_active();
            let status = reap(child)?;
            if status.is_some() {
                *active = false;
            }
            drop(active);
            Ok(status)
        }

        pub(super) fn disarm(&self) {
            *self.lock_active() = false;
        }

        fn lock_active(&self) -> MutexGuard<'_, bool> {
            self.active.lock().unwrap_or_else(PoisonError::into_inner)
        }
    }
}
