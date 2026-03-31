//! subprocess_rs — Rust subprocess engine for Hermes Agent
//!
//! Replaces Python threading-based subprocess management with a clean async interface.
//! Handles: spawn, async drain, kill, interrupt, exit codes, sudo password injection,
//! encoding, timeout. No GIL contention — all I/O runs in background threads.

use libc::{kill, setpgid, waitpid, WIFEXITED, WEXITSTATUS, WIFSIGNALED, WTERMSIG, SIGKILL};
use once_cell::sync::Lazy;
use pyo3::prelude::*;
use serde::Serialize;
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use std::{io, thread};

// Global process registry — allows interrupt/kill from anywhere in the Python call stack.
static PROCESS_REGISTRY: Lazy<Mutex<HashMap<String, Arc<SubprocessState>>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

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
// Results
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Serialize)]
pub struct ExecuteResult {
    pub output: String,
    pub returncode: i32,
    pub interrupted: bool,
    pub timed_out: bool,
}

// ─────────────────────────────────────────────────────────────────────────────
// Core spawn + drain
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

    // Unix: setpgid(0, 0) in child so we can killpg(-pid) to terminate the whole tree.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        child.pre_exec(|| {
            // SAFETY: setpgid is async-signal-safe; we're in a fresh child before exec
            if setpgid(0, 0) != 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }

    let mut proc = match child.spawn() {
        Ok(p) => p,
        Err(e) => {
            return Err(PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(format!(
                "failed to spawn '{}': {}",
                cmd[0], e
            )))
        }
    };

    let pid = proc.id();
    *state.pid.lock().unwrap() = Some(pid);
    #[cfg(unix)]
    {
        *state.pgrp.lock().unwrap() = Some(-(pid as i32));
    }

    // Write stdin then close it so sudo -S sees EOF on the password line.
    if !stdin_data.is_empty() {
        if let Some(mut stdin) = proc.stdin.take() {
            let _ = stdin.write_all(stdin_data.as_bytes());
        }
    }
    drop(proc.stdin.take()); // close stdin pipe

    // Extract pipes BEFORE moving proc into the wait thread
    let stdout_pipe = proc.stdout.take().expect("stdout captured");
    let stderr_pipe = proc.stderr.take().expect("stderr captured");

    // Register immediately so interrupt() can find it
    {
        let mut reg = PROCESS_REGISTRY.lock().unwrap();
        reg.insert(session_id.clone(), state.clone());
    }

    // Background thread: wait for exit, record code
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
        // Fallback: poll done flag
        thread::sleep(Duration::from_secs(60));
        *state_wait.exit_code.lock().unwrap() = Some(0);
        state_wait.done.store(true, Ordering::Relaxed);
    });

    // Background threads: drain stdout and stderr separately
    let state_out = state.clone();
    thread::spawn(move || {
        _drain_reader(stdout_pipe, state_out, false);
    });
    let state_err = state.clone();
    thread::spawn(move || {
        _drain_reader(stderr_pipe, state_err, true);
    });

    // Timeout kill thread
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

/// Drain a pipe into state.output using BufReader (line-by-line, non-blocking).
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

// ─────────────────────────────────────────────────────────────────────────────
// wait() — poll until done, interrupt, or deadline
// ─────────────────────────────────────────────────────────────────────────────

/// Blocking wait for subprocess completion.
#[pyfunction]
pub fn wait(py: Python<'_>, handle: &Py<SubprocessHandle>) -> PyResult<ExecuteResult> {
    let h = handle.as_ref(py);
    let poll_interval = Duration::from_millis(20);

    loop {
        // 1. Interrupt check
        if h.state.interrupted.load(Ordering::Relaxed) {
            let out = h.state.output.lock().unwrap().clone();
            return Ok(ExecuteResult {
                output: out,
                returncode: 130,
                interrupted: true,
                timed_out: false,
            });
        }

        // 2. Deadline check
        if let Some(deadline) = h.deadline {
            if Instant::now() >= deadline {
                let _ = _kill_group(&h.state);
                let out = h.state.output.lock().unwrap().clone();
                return Ok(ExecuteResult {
                    output: out,
                    returncode: 124,
                    interrupted: false,
                    timed_out: true,
                });
            }
        }

        // 3. Done check
        if h.state.done.load(Ordering::Relaxed) {
            let out = h.state.output.lock().unwrap().clone();
            let code = *h.state.exit_code.lock().unwrap();
            return Ok(ExecuteResult {
                output: out,
                returncode: code.unwrap_or(-1),
                interrupted: false,
                timed_out: false,
            });
        }

        thread::sleep(poll_interval);
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// interrupt / kill / cleanup
// ─────────────────────────────────────────────────────────────────────────────

/// Interrupt a running subprocess (SIGKILL to process group).
#[pyfunction]
pub fn interrupt(session_id: &str) -> PyResult<bool> {
    let reg = PROCESS_REGISTRY.lock().unwrap();
    if let Some(state) = reg.get(session_id) {
        state.interrupted.store(true, Ordering::Relaxed);
        let _ = _kill_group(state);
        Ok(true)
    } else {
        Ok(false)
    }
}

/// Read accumulated output so far without blocking.
#[pyfunction]
pub fn drain_partial(py: Python<'_>, handle: &Py<SubprocessHandle>) -> PyResult<String> {
    let h = handle.as_ref(py);
    Ok(h.state.output.lock().unwrap().clone())
}

/// Remove a session from the registry.
#[pyfunction]
pub fn cleanup_session(session_id: &str) -> PyResult<()> {
    let mut reg = PROCESS_REGISTRY.lock().unwrap();
    reg.remove(session_id);
    Ok(())
}

/// Called by Python wrapper to record an externally-obtained exit code.
#[pyfunction]
pub fn set_process_exited(session_id: &str, exit_code: i32) -> PyResult<()> {
    let reg = PROCESS_REGISTRY.lock().unwrap();
    if let Some(state) = reg.get(session_id) {
        *state.exit_code.lock().unwrap() = Some(exit_code);
        state.done.store(true, Ordering::Relaxed);
    }
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// Process group kill
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(unix)]
fn _kill_group(state: &Arc<SubprocessState>) -> io::Result<()> {
    let pgrp = *state.pgrp.lock().unwrap();
    if let Some(pg) = pgrp {
        // SAFETY: pg is a process group we created via setpgid in the child
        unsafe { kill(-pg, SIGKILL) };
    } else if let Some(pid) = *state.pid.lock().unwrap() {
        unsafe { kill(pid as libc::pid_t, SIGKILL) };
    }
    Ok(())
}

#[cfg(not(unix))]
fn _kill_group(state: &Arc<SubprocessState>) -> io::Result<()> {
    use std::process::Command;
    if let Some(pid) = *state.pid.lock().unwrap() {
        let _ = Command::new("taskkill")
            .args(["/F", "/T", &format!("/PID {}", pid)])
            .spawn();
    }
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// UUID v4 (no external deps)
// ─────────────────────────────────────────────────────────────────────────────

fn _uuid_v4() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos() as u64;
    let e = ts
        ^ (std::process::id() as u64 * 0x517cc1b727220a95)
        ^ ((std::thread::current().id().as_u64() as u64) * 0x8d04d2a7);
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
// Python SubprocessHandle
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

    fn is_done(&self) -> bool {
        self.state.done.load(Ordering::Relaxed)
    }

    fn get_output(&self) -> String {
        self.state.output.lock().unwrap().clone()
    }

    fn get_exit_code(&self) -> Option<i32> {
        *self.state.exit_code.lock().unwrap()
    }

    fn kill(&self) -> PyResult<()> {
        let _ = _kill_group(&self.state);
        Ok(())
    }

    fn interrupt(&self) -> PyResult<bool> {
        self.state.interrupted.store(true, Ordering::Relaxed);
        let _ = _kill_group(&self.state);
        Ok(true)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Module entry point
// ─────────────────────────────────────────────────────────────────────────────

#[pymodule]
pub fn subprocess_rs(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(spawn, m)?)?;
    m.add_function(wrap_pyfunction!(wait, m)?)?;
    m.add_function(wrap_pyfunction!(interrupt, m)?)?;
    m.add_function(wrap_pyfunction!(drain_partial, m)?)?;
    m.add_function(wrap_pyfunction!(cleanup_session, m)?)?;
    m.add_function(wrap_pyfunction!(set_process_exited, m)?)?;
    m.add_class::<SubprocessHandle>()?;
    Ok(())
}
