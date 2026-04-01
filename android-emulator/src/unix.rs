//! Unix-specific utilities for managing emulator processes
//!
//! This module provides process group integration to ensure that when we
//! kill an emulator process, all child processes (like qemu) are also terminated.

use std::io;
use tokio::process::{Child, Command};

/// A Unix process group that contains the emulator process and all its children
///
/// When this handle is explicitly killed, all processes in the group
/// are terminated, ensuring clean shutdown even when the emulator spawns
/// child processes like qemu.
#[derive(Debug)]
pub struct EmulatorProcessGroup {
    /// The process group ID, which is the same as the leader's PID
    pgid: u32,
}

impl EmulatorProcessGroup {
    /// Spawn a command in a new process group, returning both the group handle and the child process
    ///
    /// The process is configured to create a new process group (with `setpgid(0, 0)`),
    /// which ensures that all child processes spawned by the command are also part
    /// of the same group and will be killed when the group is terminated.
    pub fn spawn(mut cmd: Command) -> io::Result<(Self, Child)> {
        // Configure the command to create a new process group
        // This is equivalent to calling setpgid(0, 0) in the child
        unsafe {
            cmd.pre_exec(|| {
                // Create a new process group with this process as the leader
                // setpgid(0, 0) sets the process group ID to the process ID
                if libc::setpgid(0, 0) != 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }

        // Spawn the process
        let child = cmd.spawn()?;

        // Get the process ID, which is also the process group ID
        let pgid = child
            .id()
            .ok_or_else(|| io::Error::other("Child process has no ID"))?;

        Ok((Self { pgid }, child))
    }

    /// Explicitly terminate all processes in the process group
    ///
    /// This sends SIGTERM to all processes in the group. The negative PID
    /// tells kill(2) to send the signal to the entire process group.
    pub fn kill(&self) -> io::Result<()> {
        // Send SIGTERM to the entire process group
        // Using negative PID sends the signal to all processes in the group
        unsafe {
            if libc::kill(-(self.pgid as libc::pid_t), libc::SIGTERM) != 0 {
                return Err(io::Error::last_os_error());
            }
        }

        Ok(())
    }

    /// Get the process group ID
    #[allow(dead_code)]
    pub fn pgid(&self) -> u32 {
        self.pgid
    }
}
