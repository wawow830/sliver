//! Private systemd test ownership and unwind cleanup. Never enumerate or stop
//! production workers by the global `sliver-lua-worker-*` pattern.
use std::collections::BTreeSet;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;

use anyhow::{ensure, Context, Result};

pub(crate) fn worker_units(pid: u32) -> Result<Vec<String>> {
    ensure!(pid != 0, "worker owner PID must be nonzero");
    let prefix = format!("sliver-lua-worker-{pid}-");
    let output = Command::new("systemctl")
        .args(["--user", "list-units", "--all", "--no-legend", "--plain"])
        .arg(format!("{prefix}*.service"))
        .output()?;
    ensure!(output.status.success(), "listing test worker units failed");
    Ok(String::from_utf8(output.stdout)?
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .filter(|unit| unit.starts_with(&prefix) && unit.ends_with(".service"))
        .map(str::to_owned)
        .collect())
}

pub(crate) struct TestWorkers(BTreeSet<u32>);

impl TestWorkers {
    pub(crate) fn new(pid: u32) -> Self {
        assert_ne!(pid, 0);
        Self(BTreeSet::from([pid]))
    }
}

impl Drop for TestWorkers {
    fn drop(&mut self) {
        for pid in &self.0 {
            if let Ok(units) = worker_units(*pid) {
                for unit in units {
                    let _ = stop_unit(&unit);
                }
            }
        }
    }
}

fn stop_unit(unit: &str) -> Result<()> {
    let status = Command::new("systemctl")
        .args(["--user", "stop", unit])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    ensure!(status.success(), "stopping test unit {unit} failed");
    Ok(())
}

pub(crate) struct TestSupervisorUnit {
    unit: String,
    workers: TestWorkers,
}

impl TestSupervisorUnit {
    pub(crate) fn start(unit: String, command: &mut Command) -> Result<Self> {
        ensure!(
            unit.starts_with("sliver-supervisor-test-"),
            "not a test supervisor unit"
        );
        let mut guard = Self {
            unit,
            workers: TestWorkers(BTreeSet::new()),
        };
        let mut child = command.spawn()?;
        ensure!(child.wait()?.success(), "starting test supervisor failed");
        guard.main_pid()?;
        Ok(guard)
    }

    pub(crate) fn main_pid(&mut self) -> Result<u32> {
        let output = Command::new("systemctl")
            .args(["--user", "show", &self.unit, "-p", "MainPID", "--value"])
            .output()?;
        ensure!(
            output.status.success(),
            "reading test supervisor PID failed"
        );
        let pid = String::from_utf8(output.stdout)?.trim().parse::<u32>()?;
        ensure!(pid != 0, "test supervisor has no main PID");
        self.workers.0.insert(pid);
        Ok(pid)
    }

    pub(crate) fn stop(&self) -> Result<()> {
        stop_unit(&self.unit)
    }
}

impl Drop for TestSupervisorUnit {
    fn drop(&mut self) {
        // Capture a replacement PID even if an assertion failed immediately
        // after restart. Stop the supervisor before cleaning residual workers.
        let _ = self.main_pid();
        let _ = self.stop();
    }
}

pub(crate) struct TestBrokerThread {
    running: Arc<AtomicBool>,
    thread: Option<JoinHandle<Result<()>>>,
}

impl TestBrokerThread {
    pub(crate) fn new(running: Arc<AtomicBool>, thread: JoinHandle<Result<()>>) -> Self {
        Self {
            running,
            thread: Some(thread),
        }
    }

    pub(crate) fn finish(mut self) -> Result<()> {
        self.running.store(false, Ordering::Release);
        self.thread
            .take()
            .expect("broker thread is present")
            .join()
            .map_err(|_| anyhow::anyhow!("test broker thread panicked"))?
            .context("test broker failed")
    }
}

impl Drop for TestBrokerThread {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[test]
fn test_unit_guard_cleans_owned_workers_during_unwind() -> Result<()> {
    let _systemd_tests = crate::lock_systemd_tests();
    let available = Command::new("systemd-run")
        .args(["--user", "--wait", "--quiet", "true"])
        .status();
    if !available.is_ok_and(|status| status.success()) {
        return Ok(());
    }
    let unit = format!(
        "sliver-supervisor-test-cleanup-{}.service",
        std::process::id()
    );
    let mut command = Command::new("systemd-run");
    command.args([
        "--user",
        "--unit",
        &unit,
        "--collect",
        "--quiet",
        "--service-type=exec",
        "sleep",
        "30",
    ]);
    let mut service = TestSupervisorUnit::start(unit.clone(), &mut command)?;
    let pid = service.main_pid()?;
    // A separate transient unit deliberately outlives the fake supervisor.
    // Only the guard's recorded owner PID can identify it for cleanup.
    let worker = format!("sliver-lua-worker-{pid}-unwind.service");
    ensure!(
        Command::new("systemd-run")
            .args([
                "--user",
                "--unit",
                &worker,
                "--collect",
                "--quiet",
                "--service-type=exec",
                "sleep",
                "30"
            ])
            .status()?
            .success(),
        "starting test worker failed"
    );
    assert_eq!(worker_units(pid)?, vec![worker]);
    let result = std::panic::catch_unwind(move || {
        let _service = service;
        panic!("injected assertion failure to exercise test cleanup");
    });
    assert!(result.is_err());
    assert!(
        worker_units(pid)?.is_empty(),
        "worker survived guard unwind"
    );
    let output = Command::new("systemctl")
        .args(["--user", "show", &unit, "-p", "MainPID", "--value"])
        .output()?;
    assert_eq!(
        String::from_utf8(output.stdout)?.trim(),
        "0",
        "test supervisor survived guard unwind"
    );
    Ok(())
}
