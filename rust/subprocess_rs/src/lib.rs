//! subprocess_rs — Rust subprocess engine for Hermes Agent
//!
//! Replaces Python threading-based subprocess management with a clean async interface.
//! Handles: spawn, async drain, kill, interrupt, exit codes, sudo password injection,
//! timeout, process groups. No GIL contention — all I/O runs in background threads.

use libc::{kill as libc_kill, setpgid, waitpid, WEXITSTATUS, WIFEXITED, WIFSIGNALED, WTERMSIG, SIGKILL};
use once_cell::sync::Lazy;
use pyo3::prelude::*;
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use std::thread;

// ─────────────────────────────────────────────────────────────────────────────
// Global process registry
// ─────────────────────────────────────────────────────────────────────────────

static PROCESS_REGISTRY: Lazy<Mutex<HashMap<String, Arc<SubprocessState>>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

// Simple monotonic counter for UUID substitute (no thread::id needed)
static UUID_COUNTER: AtomicU64 = AtomicU64::new(1);

// ─────────────────────────────────────────────────────────────────────────────
// Shared subprocess state
// ─────────────────────────────────────────────────────────────────────────────

struct SubprocessState {
    interrupted: AtomicBool,
    pid: Mutex<Option<u32>>,
    pgrp: Mutex<Option<i32>>,
    done: AtomicBool,
    output: Mutex<String>,
    exit_code: Mutex<Option<i32>>,
}

impl SubprocessState {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            interrupted: AtomicBool::new(false),
            pid: Mutex::new(None),
            pgrp: Mutex::new(None),
            done: AtomicBool::new(false),
            output: Mutex::new(String::new()),
            exit_code: Mutex::new(None),
        })
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// ExecuteResult — Python-facing return type
// ─────────────────────────────────────────────────────────────────────────────

/// Result of a completed subprocess execution.
#[pyclass]
pub struct ExecuteResult {
    #[pyo3(get)]
    pub output: String,
    #[pyo3(get)]
    pub returncode: i32,
    #[pyo3(get)]
    pub interrupted: bool,
    #[pyo3(get)]
    pub timed_out: bool,
}

// ─────────────────────────────────────────────────────────────────────────────
// SubprocessHandle — Python-facing handle for a running process
// ─────────────────────────────────────────────────────────────────────────────

#[pyclass]
pub struct SubprocessHandle {
    session_id: String,
    state: Arc<SubprocessState>,
    deadline: Option<Instant>,
}

#[pymethods]
impl SubprocessHandle {
    #[getter]
    fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Blocking wait — polls until done, interrupted, or deadline.
    /// Returns an ExecuteResult.
    fn wait(&self) -> ExecuteResult {
        let poll_interval = Duration::from_millis(20);

        loop {
            if self.state.interrupted.load(Ordering::Relaxed) {
                return ExecuteResult {
                    output: self.state.output.lock().unwrap().clone(),
                    returncode: 130,
                    interrupted: true,
                    timed_out: false,
                };
            }

            if let Some(deadline) = self.deadline {
                if Instant::now() >= deadline {
                    let _ = _kill_group(&self.state);
                    return ExecuteResult {
                        output: self.state.output.lock().unwrap().clone(),
                        returncode: 124,
                        interrupted: false,
                        timed_out: true,
                    };
                }
            }

            if self.state.done.load(Ordering::Relaxed) {
                return ExecuteResult {
                    output: self.state.output.lock().unwrap().clone(),
                    returncode: self.state.exit_code.lock().unwrap().unwrap_or(-1),
                    interrupted: false,
                    timed_out: false,
                };
            }

            thread::sleep(poll_interval);
        }
    }

    /// Read accumulated output so far without blocking.
    fn drain_partial(&self) -> String {
        self.state.output.lock().unwrap().clone()
    }

    /// True if the process has exited.
    fn is_done(&self) -> bool {
        self.state.done.load(Ordering::Relaxed)
    }

    /// Kill the process and all its children.
    fn kill(&self) {
        let _ = _kill_group(&self.state);
    }

    /// Interrupt the process (sets interrupted flag + kills).
    fn interrupt(&self) {
        self.state.interrupted.store(true, Ordering::Relaxed);
        let _ = _kill_group(&self.state);
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Module-level functions
// ─────────────────────────────────────────────────────────────────────────────

/// Spawn a subprocess and begin draining its output in background threads.
///
/// Args:
///   cmd: vec of arg strings (shell=False)
///   cwd: working directory (empty = inherit from parent)
///   timeout_ms: deadline in milliseconds (0 = no timeout)
///   stdin_data: string to write to stdin then close (sudo password + user data)
///   env: extra env vars to merge with inherited env
#[pyfunction]
pub fn spawn(
    py: Python<'_>,
    cmd: Vec<String>,
    cwd: String,
    timeout_ms: u64,
    stdin_data: String,
    env: HashMap<String, String>,
) -> PyResult<Py<SubprocessHandle>> {
    let session_id = _uuid_v4();
    let state = SubprocessState::new();

    let mut child = Command::new(&cmd[0]);
    child.args(&cmd[1..]);
    if !cwd.is_empty() {
        child.current_dir(&cwd);
    }

    // Merge env vars (inherited + extra)
    let mut full_env: HashMap<String, String> = std::env::vars().collect();
    full_env.extend(env);
    child.envs(&full_env);

    child.stdin(Stdio::piped());
    child.stdout(Stdio::piped());
    child.stderr(Stdio::piped());

    // Unix: setpgid(0, 0) in child before exec so we can kill the whole process group.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: pre_exec runs synchronously in the child before execve,
        // setpgid is async-signal-safe, and no other threads exist in the child yet.
        // SAFETY: pre_exec runs synchronously in the child before execve.
        // setpgid is async-signal-safe and no other threads exist in the child yet.
        unsafe {
            child.pre_exec(|| {
                let _ = setpgid(0, 0);
                Ok(())
            });
        }
    }

    let mut proc = match child.spawn() {
        Ok(p) => p,
        Err(e) => {
            return Err(PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(format!(
                "failed to spawn '{}': {}",
                cmd[0], e
            )));
        }
    };

    let pid = proc.id();
    *state.pid.lock().unwrap() = Some(pid);
    #[cfg(unix)]
    {
        *state.pgrp.lock().unwrap() = Some(-(pid as i32));
    }

    // Write stdin then close it (sends sudo password via -S)
    if !stdin_data.is_empty() {
        if let Some(mut stdin) = proc.stdin.take() {
            let _ = stdin.write_all(stdin_data.as_bytes());
        }
    }
    drop(proc.stdin.take());

    // Extract pipes before waiting
    let stdout_pipe = proc.stdout.take().expect("stdout captured");
    let stderr_pipe = proc.stderr.take().expect("stderr captured");

    // Register immediately so interrupt() can find this process
    {
        let mut reg = PROCESS_REGISTRY.lock().unwrap();
        reg.insert(session_id.clone(), state.clone());
    }

    // Background: wait for exit, record code
    let state_wait = state.clone();
    #[cfg(unix)]
    thread::spawn(move || {
        let mut status: libc::c_int = 0;
        let ret = unsafe { waitpid(pid as libc::pid_t, &mut status, 0) };
        if ret < 0 {
            *state_wait.exit_code.lock().unwrap() = Some(-1);
        } else if WIFEXITED(status) {
            *state_wait.exit_code.lock().unwrap() = Some(WEXITSTATUS(status));
        } else if WIFSIGNALED(status) {
            *state_wait.exit_code.lock().unwrap() = Some(128 + WTERMSIG(status) as i32);
        } else {
            *state_wait.exit_code.lock().unwrap() = Some(-1);
        }
        state_wait.done.store(true, Ordering::Relaxed);
    });

    #[cfg(not(unix))]
    thread::spawn(move || {
        thread::sleep(Duration::from_secs(60));
        *state_wait.exit_code.lock().unwrap() = Some(0);
        state_wait.done.store(true, Ordering::Relaxed);
    });

    // Background: drain stdout
    let state_out = state.clone();
    thread::spawn(move || {
        _drain_reader(stdout_pipe, state_out, false);
    });

    // Background: drain stderr
    let state_err = state.clone();
    thread::spawn(move || {
        _drain_reader(stderr_pipe, state_err, true);
    });

    // Background: timeout kill
    if timeout_ms > 0 {
        let state_kill = state.clone();
        let deadline = Instant::now() + Duration::from_millis(timeout_ms);
        thread::spawn(move || {
            loop {
                if state_kill.interrupted.load(Ordering::Relaxed) {
                    return;
                }
                if Instant::now() >= deadline {
                    let _ = _kill_group(&state_kill);
                    return;
                }
                thread::sleep(Duration::from_millis(20));
            }
        });
    }

    let deadline = if timeout_ms > 0 {
        Some(Instant::now() + Duration::from_millis(timeout_ms))
    } else {
        None
    };

    let handle = Py::new(
        py,
        SubprocessHandle {
            session_id: session_id.clone(),
            state,
            deadline,
        },
    )?;

    Ok(handle)
}

/// Interrupt a running subprocess by session ID (SIGKILL to process group).
#[pyfunction]
pub fn interrupt(session_id: &str) -> bool {
    let reg = PROCESS_REGISTRY.lock().unwrap();
    if let Some(state) = reg.get(session_id) {
        state.interrupted.store(true, Ordering::Relaxed);
        let _ = _kill_group(state);
        true
    } else {
        false
    }
}

/// Remove a session from the registry.
#[pyfunction]
pub fn cleanup_session(session_id: &str) {
    let mut reg = PROCESS_REGISTRY.lock().unwrap();
    reg.remove(session_id);
}

/// Called by Python wrapper to record an externally-obtained exit code.
#[pyfunction]
pub fn set_process_exited(session_id: &str, exit_code: i32) {
    let reg = PROCESS_REGISTRY.lock().unwrap();
    if let Some(state) = reg.get(session_id) {
        *state.exit_code.lock().unwrap() = Some(exit_code);
        state.done.store(true, Ordering::Relaxed);
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Internal helpers
// ─────────────────────────────────────────────────────────────────────────────

fn _drain_reader<R: Read + Send + 'static>(reader: R, state: Arc<SubprocessState>, is_stderr: bool) {
    let mut buf_reader = BufReader::new(reader);
    let mut line = String::new();
    loop {
        match buf_reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {
                let mut out = state.output.lock().unwrap();
                if is_stderr {
                    out.push_str("[stderr] ");
                }
                out.push_str(&line);
                line.clear();
            }
            Err(_) => break,
        }
    }
}

#[cfg(unix)]
fn _kill_group(state: &Arc<SubprocessState>) {
    let pgrp = *state.pgrp.lock().unwrap();
    if let Some(pg) = pgrp {
        // SAFETY: pg is a process group we created via setpgid in the child;
        // SIGKILL to a process group kills all members safely.
        unsafe { libc_kill(-pg, SIGKILL) };
    } else if let Some(pid) = *state.pid.lock().unwrap() {
        unsafe { libc_kill(pid as libc::pid_t, SIGKILL) };
    }
}

#[cfg(not(unix))]
fn _kill_group(state: &Arc<SubprocessState>) {
    use std::process::Command;
    if let Some(pid) = *state.pid.lock().unwrap() {
        let _ = Command::new("taskkill")
            .args(["/F", "/T", &format!("/PID {}", pid)])
            .spawn();
    }
}

fn _uuid_v4() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos() as u128;
    let counter = UUID_COUNTER.fetch_add(1, Ordering::Relaxed) as u128;
    let pid = std::process::id() as u128;
    // Use u128 to avoid overflow in the mixing step
    let e = ts ^ (pid * 0x517cc1b727220a95u128) ^ (counter * 0x8d04d2a7u128);
    format!(
        "{:08x}-{:04x}-4{:03x}-{:04x}-{:012x}",
        (e & 0xffffffff) as u32,
        ((e >> 32) & 0xffff) as u16,
        ((e >> 48) & 0x0fff) as u16,
        (((e >> 60) & 0x3fff) as u16) | 0x8000,
        ((e >> 4) & 0xffffffffffff) as u64
    )
}

// ─────────────────────────────────────────────────────────────────────────────
// Module entry point
// ─────────────────────────────────────────────────────────────────────────────

#[pymodule]
pub fn _subprocess_rs(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(spawn, m)?)?;
    m.add_function(wrap_pyfunction!(interrupt, m)?)?;
    m.add_function(wrap_pyfunction!(cleanup_session, m)?)?;
    m.add_function(wrap_pyfunction!(set_process_exited, m)?)?;
    m.add_class::<SubprocessHandle>()?;
    m.add_class::<ExecuteResult>()?;
    Ok(())
}
