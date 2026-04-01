//! Windows-specific utilities for managing emulator processes
//!
//! This module provides Windows Job Object integration to ensure that when we
//! kill an emulator.exe process, all child processes (like qemu) are also terminated.

use std::io;
use tokio::process::{Child, Command};

/// A Windows Job Object that contains the emulator process and all its children
///
/// When this handle is dropped or explicitly killed, all processes in the job
/// are terminated, ensuring clean shutdown even when emulator.exe spawns
/// child processes like qemu.
#[derive(Debug)]
pub struct EmulatorJob {
    job: win32job::Job,
}

impl EmulatorJob {
    /// Spawn a command in a new job object, returning both the job and the child process
    ///
    /// The process is created in a suspended state, assigned to a new job object,
    /// and then resumed. This ensures that all child processes spawned by the
    /// command are also part of the job and will be killed when the job is terminated.
    pub fn spawn(mut cmd: Command) -> io::Result<(Self, Child)> {
        use windows::Win32::Foundation::CloseHandle;
        use windows::Win32::System::Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
        };
        use windows::Win32::System::Threading::{
            CREATE_SUSPENDED, OpenProcess, OpenThread, PROCESS_SET_QUOTA, PROCESS_TERMINATE,
            ResumeThread, THREAD_SUSPEND_RESUME,
        };

        // Create the job object first
        let job = win32job::Job::create().map_err(|e| {
            io::Error::new(io::ErrorKind::Other, format!("Failed to create job: {}", e))
        })?;

        // Configure the job to kill all processes when the job handle is closed
        let mut info = job.query_extended_limit_info().map_err(|e| {
            io::Error::new(
                io::ErrorKind::Other,
                format!("Failed to query job info: {}", e),
            )
        })?;

        info.limit_kill_on_job_close();

        job.set_extended_limit_info(&mut info).map_err(|e| {
            io::Error::new(
                io::ErrorKind::Other,
                format!("Failed to set job info: {}", e),
            )
        })?;

        // Spawn the process in suspended state so we can assign it to the job before qemu starts
        cmd.creation_flags(CREATE_SUSPENDED.0);
        let child = cmd.spawn()?;

        // Get the process ID and assign to job
        let process_id = child
            .id()
            .ok_or_else(|| io::Error::new(io::ErrorKind::Other, "Child process has no ID"))?;

        // Open the process handle and assign to job
        unsafe {
            let h_process = OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, false, process_id)
                .map_err(|e| {
                io::Error::new(
                    io::ErrorKind::Other,
                    format!("Failed to open process: {}", e),
                )
            })?;

            job.assign_process(h_process.0 as _).map_err(|e| {
                let _ = CloseHandle(h_process);
                io::Error::new(
                    io::ErrorKind::Other,
                    format!("Failed to assign process to job: {}", e),
                )
            })?;

            let _ = CloseHandle(h_process);
        }

        // Resume the primary thread now that the process is in the job
        unsafe {
            // Use Tool Help API to find the main thread
            let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0).map_err(|e| {
                io::Error::new(
                    io::ErrorKind::Other,
                    format!("Failed to create snapshot: {}", e),
                )
            })?;

            let mut thread_entry: THREADENTRY32 = std::mem::zeroed();
            thread_entry.dwSize = std::mem::size_of::<THREADENTRY32>() as u32;

            let mut thread_id = None;
            if Thread32First(snapshot, &mut thread_entry).is_ok() {
                loop {
                    if thread_entry.th32OwnerProcessID == process_id {
                        thread_id = Some(thread_entry.th32ThreadID);
                        break;
                    }
                    if Thread32Next(snapshot, &mut thread_entry).is_err() {
                        break;
                    }
                }
            }
            let _ = CloseHandle(snapshot);

            if let Some(tid) = thread_id {
                let thread_handle = OpenThread(THREAD_SUSPEND_RESUME, false, tid).map_err(|e| {
                    io::Error::new(
                        io::ErrorKind::Other,
                        format!("Failed to open thread: {}", e),
                    )
                })?;

                let _ = ResumeThread(thread_handle);
                let _ = CloseHandle(thread_handle);
            } else {
                return Err(io::Error::new(
                    io::ErrorKind::Other,
                    "Could not find main thread to resume",
                ));
            }
        }

        Ok((Self { job }, child))
    }

    /// Explicitly terminate all processes in the job
    pub fn kill(&self) -> io::Result<()> {
        use windows::Win32::Foundation::HANDLE;
        use windows::Win32::System::JobObjects::TerminateJobObject;

        // Get the raw job handle and terminate using Win32 API directly
        let job_handle = self.job.handle() as *mut std::ffi::c_void;

        unsafe {
            TerminateJobObject(HANDLE(job_handle), 1).map_err(|e| {
                io::Error::new(
                    io::ErrorKind::Other,
                    format!("Failed to terminate job: {}", e),
                )
            })?;
        }

        Ok(())
    }
}
