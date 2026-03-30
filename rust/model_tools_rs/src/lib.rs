use lazy_static::lazy_static;
use pyo3::exceptions::PyException;
use pyo3::prelude::*;
use pyo3::types::{PyAnyMethods, PyDict, PyList, PyListMethods, PyModuleMethods, PySet};
use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, atomic::{AtomicBool, Ordering}};

// Legacy toolset mapping
fn legacy_toolset_map() -> HashMap<&'static str, Vec<&'static str>> {
    HashMap::from([
        ("web_tools", vec!["web_search", "web_extract"]),
        ("terminal_tools", vec!["terminal"]),
        ("vision_tools", vec!["vision_analyze"]),
        ("moa_tools", vec!["mixture_of_agents"]),
        ("image_tools", vec!["image_generate"]),
        (
            "skills_tools",
            vec!["skills_list", "skill_view", "skill_manage"],
        ),
        (
            "browser_tools",
            vec![
                "browser_navigate",
                "browser_snapshot",
                "browser_click",
                "browser_type",
                "browser_scroll",
                "browser_back",
                "browser_press",
                "browser_close",
                "browser_get_images",
                "browser_vision",
                "browser_console",
            ],
        ),
        ("cronjob_tools", vec!["cronjob"]),
        (
            "rl_tools",
            vec![
                "rl_list_environments",
                "rl_select_environment",
                "rl_get_current_config",
                "rl_edit_config",
                "rl_start_training",
                "rl_check_status",
                "rl_stop_training",
                "rl_get_results",
                "rl_list_runs",
                "rl_test_inference",
            ],
        ),
        (
            "file_tools",
            vec!["read_file", "write_file", "patch", "search_files"],
        ),
        ("tts_tools", vec!["text_to_speech"]),
    ])
}

// -------------------------------------------------------------------------------------------------
// Rust-native ToolRegistry — stores schemas as Py<PyDict> (no JSON roundtrip in hot path)
// -------------------------------------------------------------------------------------------------

#[allow(dead_code)]
struct RustToolEntry {
    name: String,
    toolset: String,
    /// Owned reference to the Python function schema dict (stored as Py<PyAny> for lifetime compatibility).
    /// Stored directly — no JSON conversion needed at return time.
    schema: Py<PyAny>,
    check_fn: Option<Py<PyAny>>,
    requires_env: Vec<String>,
    is_async: bool,
    description: String,
    emoji: String,
}

lazy_static! {
    static ref RUST_TOOL_REGISTRY: Mutex<HashMap<String, RustToolEntry>> =
        Mutex::new(HashMap::new());
    // Cached Python module references — resolved once at init, reused every call
    static ref CACHED_REGISTRY_MOD: Mutex<Option<Py<PyAny>>> = Mutex::new(None);
    // Cached invoke_hook — resolved once at init, reused every call
    static ref CACHED_INVOKE_HOOK: Mutex<Option<Py<PyAny>>> = Mutex::new(None);
    // Python callback to update _last_resolved_tool_names in the Python module
    static ref SET_LAST_RESOLVED_CALLBACK: Mutex<Option<Py<PyAny>>> = Mutex::new(None);
}

static INITIALIZED: AtomicBool = AtomicBool::new(false);

fn json_error(message: String) -> String {
    serde_json::json!({ "error": message }).to_string()
}

fn py_print(py: Python<'_>, message: String) -> PyResult<()> {
    py.import("builtins")?.getattr("print")?.call1((message,))?;
    Ok(())
}

fn logger_call(py: Python<'_>, level: &str, message: &str) {
    if let Ok(logging) = py.import("logging") {
        if let Ok(logger) = logging.call_method1("getLogger", ("model_tools",)) {
            let _ = logger.call_method1(level, (message,));
        }
    }
}

fn registry_obj(py: Python<'_>) -> PyResult<Py<PyAny>> {
    let registry_mod = py.import("tools.registry")?;
    Ok(registry_mod.getattr("registry")?.into())
}

// -------------------------------------------------------------------------------------------------
// Cached data structures
// -------------------------------------------------------------------------------------------------

lazy_static! {
    static ref TOOLSET_RESOLVED: Mutex<HashMap<String, Vec<String>>> = Mutex::new(HashMap::new());
    static ref TOOL_TO_TOOLSET_MAP: Mutex<HashMap<String, String>> = Mutex::new(HashMap::new());
    static ref ALL_TOOLS: Mutex<HashSet<String>> = Mutex::new(HashSet::new());
}

/// Returns a cached reference to tools.registry.registry.
/// Cached in CACHED_REGISTRY_MOD — resolved once at init, reused every call.
#[allow(dead_code)]
#[inline]
fn get_cached_registry(py: Python<'_>) -> PyResult<Bound<'_, PyAny>> {
    let guard = CACHED_REGISTRY_MOD.lock().unwrap();
    if let Some(ref mod_any) = *guard {
        return Ok(mod_any.bind(py).clone());
    }
    drop(guard);
    let registry_mod = py.import("tools.registry")?;
    let registry = registry_mod.getattr("registry")?;
    let mut guard = CACHED_REGISTRY_MOD.lock().unwrap();
    *guard = Some(registry.into_any().into());
    Ok(CACHED_REGISTRY_MOD
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .bind(py)
        .clone())
}

/// Returns the cached invoke_hook from hermes_cli.plugins.
/// Resolved once on first call, reused every call thereafter.
#[allow(dead_code)]
#[inline]
fn get_invoke_hook(py: Python<'_>) -> Option<Bound<'_, PyAny>> {
    let guard = CACHED_INVOKE_HOOK.lock().unwrap();
    if let Some(ref hook) = *guard {
        return Some(hook.bind(py).clone());
    }
    drop(guard);
    let hook: Option<Bound<'_, PyAny>> = py
        .import("hermes_cli.plugins")
        .ok()?
        .getattr("invoke_hook")
        .ok();
    if let Some(ref h) = hook {
        let mut guard = CACHED_INVOKE_HOOK.lock().unwrap();
        *guard = Some(h.clone().into());
    }
    hook
}

// -------------------------------------------------------------------------------------------------
// get_tool_definitions (hot path — no JSON roundtrip)
// Returns Py<PyList> directly: each item is a Python dict with {type, function: schema}.
// -------------------------------------------------------------------------------------------------

/// Returns (filtered_tools: Py<PyList>, available_names: Vec<String>)
/// Caller owns the returned Py<PyList>.
#[inline]
fn rs_get_definitions_inner(
    py: Python<'_>,
    tool_names: &HashSet<String>,
    quiet: bool,
) -> PyResult<(Py<PyList>, Vec<String>)> {
    let registry = RUST_TOOL_REGISTRY.lock().unwrap();
    let mut check_cache: HashMap<String, bool> = HashMap::new();

    let mut sorted_names: Vec<&String> = tool_names.iter().collect();
    sorted_names.sort();

    // Pre-allocate result list
    let results = PyList::empty(py);
    let mut available_names: Vec<String> = Vec::new();

    for name in sorted_names {
        let entry = match registry.get(name) {
            Some(e) => e,
            None => continue,
        };

        let available = if let Some(ref check_fn) = entry.check_fn {
            if let Some(&v) = check_cache.get(name) {
                v
            } else {
                let result = check_fn.call0(py);
                let ok = result.and_then(|r| r.is_truthy(py)).unwrap_or(false);
                check_cache.insert(name.clone(), ok);
                ok
            }
        } else {
            true
        };

        if !available {
            if !quiet {
                logger_call(
                    py,
                    "debug",
                    &format!("Tool {} unavailable (check failed)", name),
                );
            }
            continue;
        }

        let wrapped = PyDict::new(py);
        wrapped.set_item("type", "function")?;
        wrapped.set_item("function", entry.schema.as_ref())?;
        results.append(wrapped)?;
        available_names.push(name.clone());
    }

    Ok((results.into(), available_names))
}

// -------------------------------------------------------------------------------------------------
// initialize
// -------------------------------------------------------------------------------------------------

/// Lazily initialize tool registry on first get_tool_definitions() call.
/// This avoids eagerly importing all tools.* modules on process startup,
/// cutting cold-start from ~500ms to ~0ms.
fn ensure_initialized(py: Python<'_>) -> PyResult<()> {
    if INITIALIZED.swap(true, Ordering::SeqCst) {
        return Ok(());
    }

    let modules = vec![
        "tools.web_tools",
        "tools.terminal_tool",
        "tools.file_tools",
        "tools.vision_tools",
        "tools.mixture_of_agents_tool",
        "tools.image_generation_tool",
        "tools.minimax_image_tool",
        "tools.skills_tool",
        "tools.skill_manager_tool",
        "tools.browser_tool",
        "tools.cronjob_tools",
        "tools.rl_training_tool",
        "tools.tts_tool",
        "tools.todo_tool",
        "tools.memory_tool",
        "tools.session_search_tool",
        "tools.clarify_tool",
        "tools.code_execution_tool",
        "tools.delegate_tool",
        "tools.process_registry",
        "tools.send_message_tool",
        "tools.honcho_tools",
        "tools.homeassistant_tool",
    ];

    let importlib = py.import("importlib")?;
    for mod_name in modules {
        if let Err(err) = importlib.call_method1("import_module", (mod_name,)) {
            logger_call(
                py,
                "debug",
                &format!("Could not import tool module {}: {}", mod_name, err),
            );
        }
    }

    if let Ok(mcp_tool) = py.import("tools.mcp_tool") {
        if let Ok(discover) = mcp_tool.getattr("discover_mcp_tools") {
            let _ = discover.call0();
        }
    }

    if let Ok(plugins) = py.import("hermes_cli.plugins") {
        if let Ok(discover) = plugins.getattr("discover_plugins") {
            let _ = discover.call0();
        }
    }

    init_toolset_cache(py)?;
    Ok(())
}

#[pyfunction]
fn initialize(_py: Python<'_>) -> PyResult<()> {
    // Backward-compat stub — all initialization is now lazy on first
    // get_tool_definitions() call. No-op.
    Ok(())
}

fn init_toolset_cache(py: Python<'_>) -> PyResult<()> {
    let mut resolved_lock = TOOLSET_RESOLVED.lock().unwrap();
    let mut all_tools_lock = ALL_TOOLS.lock().unwrap();
    resolved_lock.clear();
    all_tools_lock.clear();

    let toolsets = py.import("toolsets")?;
    let all_toolsets: Bound<'_, PyDict> = toolsets
        .call_method0("get_all_toolsets")?
        .cast::<PyDict>()?
        .clone();
    for (key, _value) in all_toolsets.iter() {
        let ts_name = key.extract::<String>()?;
        let resolved = toolsets.call_method1("resolve_toolset", (ts_name.clone(),))?;
        // resolve_toolset may return a dict of {tool_name: schema} or a list of tool names
        let vec: Vec<String> = if resolved.extract::<Vec<String>>().is_ok() {
            resolved.extract()?
        } else {
            // It's a dict — extract keys
            let dict: Bound<'_, PyDict> = resolved.cast::<PyDict>()?.clone();
            dict.keys().iter().map(|k| k.extract::<String>()).collect::<PyResult<Vec<String>>>()?
        };
        resolved_lock.insert(ts_name.clone(), vec.clone());
        for tool in vec {
            all_tools_lock.insert(tool);
        }
    }

    let mut map_lock = TOOL_TO_TOOLSET_MAP.lock().unwrap();
    let mut rust_registry = RUST_TOOL_REGISTRY.lock().unwrap();
    map_lock.clear();
    rust_registry.clear();

    let registry = registry_obj(py)?;
    let map = registry.bind(py).call_method0("get_tool_to_toolset_map")?;
    let py_map: HashMap<String, String> = map.extract()?;
    *map_lock = py_map;

    let all_names: Vec<String> = registry
        .bind(py)
        .call_method0("get_all_tool_names")?
        .extract()?;
    for name in all_names {
        let entry_dict: Bound<'_, PyAny> = registry
            .bind(py)
            .getattr("_tools")?
            .get_item(name)?;
        let py_name: String = entry_dict.getattr("name")?.extract()?;
        let py_toolset: String = entry_dict.getattr("toolset")?.extract()?;
        let py_schema_any: Bound<'_, PyAny> = entry_dict.getattr("schema")?.into_any();
        let py_check_fn: Py<PyAny> = entry_dict.getattr("check_fn")?.into_any().into();
        let py_requires_env: Vec<String> = entry_dict.getattr("requires_env")?.extract()?;
        let py_is_async: bool = entry_dict.getattr("is_async")?.extract()?;
        let py_desc: String = entry_dict.getattr("description")?.extract()?;
        let py_emoji: String = entry_dict.getattr("emoji")?.extract()?;

        let check_fn_py = if py_check_fn.is_none(py) {
            None
        } else {
            Some(py_check_fn)
        };

        rust_registry.insert(
            py_name.clone(),
            RustToolEntry {
                name: py_name,
                toolset: py_toolset,
                schema: py_schema_any.into(),
                check_fn: check_fn_py,
                requires_env: py_requires_env,
                is_async: py_is_async,
                description: py_desc,
                emoji: py_emoji,
            },
        );
    }

    for name in rust_registry.keys() {
        all_tools_lock.insert(name.clone());
    }

    Ok(())
}

// -------------------------------------------------------------------------------------------------
// get_tool_definitions (public API — called from Python model_tools.py)
// -------------------------------------------------------------------------------------------------

#[pyfunction(signature = (enabled_toolsets=None, disabled_toolsets=None, quiet_mode=false))]
fn get_tool_definitions(
    py: Python<'_>,
    enabled_toolsets: Option<Vec<String>>,
    disabled_toolsets: Option<Vec<String>>,
    quiet_mode: bool,
) -> PyResult<Py<PyList>> {
    // Lazy init — discover tools and build caches on first actual request
    ensure_initialized(py)?;

    let legacy = legacy_toolset_map();

    let resolved_map = TOOLSET_RESOLVED.lock().unwrap();
    let all_tools = ALL_TOOLS.lock().unwrap();

    let mut tools_to_include = HashSet::new();

    if let Some(enabled) = enabled_toolsets.filter(|v| !v.is_empty()) {
        for toolset_name in enabled {
            if let Some(tools) = resolved_map.get(&toolset_name) {
                tools_to_include.extend(tools.iter().cloned());
                continue;
            }
            if let Some(legacy_tools) = legacy.get(toolset_name.as_str()) {
                for tool in legacy_tools {
                    tools_to_include.insert((*tool).to_string());
                }
                continue;
            }
            if !quiet_mode {
                py_print(py, format!("⚠️  Unknown toolset: {}", toolset_name))?;
            }
        }
    } else if let Some(disabled) = disabled_toolsets.filter(|v| !v.is_empty()) {
        tools_to_include = all_tools.clone();
        for toolset_name in disabled {
            if let Some(tools) = resolved_map.get(&toolset_name) {
                for tool in tools {
                    tools_to_include.remove(tool);
                }
                continue;
            }
            if let Some(legacy_tools) = legacy.get(toolset_name.as_str()) {
                for tool in legacy_tools {
                    tools_to_include.remove(*tool);
                }
                continue;
            }
            if !quiet_mode {
                py_print(py, format!("⚠️  Unknown toolset: {}", toolset_name))?;
            }
        }
    } else {
        tools_to_include = all_tools.clone();
    }
    drop(resolved_map);
    drop(all_tools);

    // rs_get_definitions_inner returns (filtered_tools, available_names)
    // Zero JSON overhead — schemas are Py<PyDict> stored directly
    let (filtered_tools, available_tool_names) =
        rs_get_definitions_inner(py, &tools_to_include, quiet_mode)?;

    // --- Special case: execute_code dynamic schema ---
    if available_tool_names.iter().any(|s| s == "execute_code") {
        let code_execution_tool = py.import("tools.code_execution_tool")?;
        let sandbox_allowed = code_execution_tool.getattr("SANDBOX_ALLOWED_TOOLS")?;
        let sandbox_enabled: Vec<String> = sandbox_allowed
            .try_iter()?
            .filter_map(|item| item.ok())
            .filter_map(|item| item.extract::<String>().ok())
            .filter(|tool_name| available_tool_names.contains(tool_name))
            .collect();
        let sandbox_set = PySet::new(py, sandbox_enabled.into_iter())?;
        let dynamic_schema: Bound<'_, PyAny> =
            code_execution_tool.call_method1("build_execute_code_schema", (sandbox_set,))?;

        // Iterate and find/replace execute_code entry
        // Convert Py<PyList> to Bound<PyList> using into_bound
        let list_bound = filtered_tools.cast_bound::<PyList>(py)?;
        for i in 0..list_bound.len() {
            let td: Bound<'_, PyAny> = list_bound.get_item(i)?;
            let function = td.get_item("function")?;
            if function.get_item("name")?.extract::<String>()? == "execute_code" {
                let replacement = PyDict::new(py);
                replacement.set_item("type", "function")?;
                replacement.set_item("function", dynamic_schema)?;
                list_bound.set_item(i, replacement)?;
                break;
            }
        }
    }

    // --- Special case: browser_navigate standalone description fix ---
    if available_tool_names.iter().any(|s| s == "browser_navigate")
        && !available_tool_names.iter().any(|s| s == "web_search")
        && !available_tool_names.iter().any(|s| s == "web_extract")
    {
        let list_bound = filtered_tools.cast_bound::<PyList>(py)?;
        for i in 0..list_bound.len() {
            let td: Bound<'_, PyAny> = list_bound.get_item(i)?;
            let function = td.get_item("function")?;
            if function.get_item("name")?.extract::<String>()? == "browser_navigate" {
                let desc: String = function.get_item("description")?.extract()?;
                let fixed = desc
                    .replace(
                        " For simple information retrieval, prefer web_search or web_extract (faster, cheaper).",
                        "",
                    );
                let replacement_function: Bound<'_, PyAny> = function.call_method0("copy")?;
                replacement_function.set_item("description", fixed)?;
                let replacement = PyDict::new(py);
                replacement.set_item("type", "function")?;
                replacement.set_item("function", replacement_function)?;
                list_bound.set_item(i, replacement)?;
                break;
            }
        }
    }

    if !quiet_mode {
        let count = available_tool_names.len();
        if count == 0 {
            py_print(
                py,
                "🛠️  No tools selected (all filtered out or unavailable)".to_string(),
            )?;
        } else {
            let mut names: Vec<&str> = available_tool_names.iter().map(|s| s.as_str()).collect();
            names.sort();
            py_print(
                py,
                format!(
                    "🛠️  Final tool selection ({} tools): {}",
                    count,
                    names.join(", ")
                ),
            )?;
        }
    }

    // Invoke Python callback to update _last_resolved_tool_names
    if let Some(ref cb) = *SET_LAST_RESOLVED_CALLBACK.lock().unwrap() {
        let py_names: Vec<&str> = available_tool_names.iter().map(|s| s.as_str()).collect();
        let py_list = PyList::new(py, &py_names)?;
        if let Err(e) = cb.call1(py, (py_list,)) {
            logger_call(py, "warning", &format!("set_last_resolved_callback failed: {}", e));
        }
    }

    Ok(filtered_tools)
}

// -------------------------------------------------------------------------------------------------
// handle_function_call
// -------------------------------------------------------------------------------------------------

#[pyfunction(signature = (function_name, function_args, task_id=None, user_task=None, enabled_tools=None, last_resolved_tool_names=None, honcho_manager=None, honcho_session_key=None))]
fn handle_function_call(
    py: Python<'_>,
    function_name: String,
    function_args: Py<PyAny>,
    task_id: Option<String>,
    user_task: Option<String>,
    enabled_tools: Option<Vec<String>>,
    last_resolved_tool_names: Option<Vec<String>>,
    honcho_manager: Option<Py<PyAny>>,
    honcho_session_key: Option<String>,
) -> PyResult<String> {
    // Suppress unused warnings for parameters not yet wired up in Rust dispatch
    let _ = (&function_args, &user_task, &enabled_tools, &last_resolved_tool_names, &honcho_manager, &honcho_session_key);
    let read_search_tools: HashSet<&str> = HashSet::from(["read_file", "search_files"]);
    let agent_loop_tools: HashSet<&str> =
        HashSet::from(["todo", "memory", "session_search", "delegate_task"]);

    if !read_search_tools.contains(function_name.as_str()) {
        if let Ok(file_tools) = py.import("tools.file_tools") {
            if let Ok(notify) = file_tools.getattr("notify_other_tool_call") {
                let _ = notify.call1((task_id.clone().unwrap_or_else(|| "default".to_string()),));
            }
        }
    }

    if agent_loop_tools.contains(function_name.as_str()) {
        return Ok(json_error(format!(
            "{} must be handled by the agent loop",
            function_name
        )));
    }

    // handle_function_call delegated to Python — raises NotImplementedError so the
    // Python caller falls back to its own registry.dispatch().
    // This avoids the GIL deadlock from Rust calling Python while the GIL is held.
    Err(PyErr::new::<pyo3::exceptions::PyNotImplementedError, _>(
        "Rust handle_function_call not implemented — use Python dispatch".to_owned(),
    ))
}


// -------------------------------------------------------------------------------------------------
// refresh_toolset_cache — clears stale caches and rebuilds from current registry state.
// Called from Python after MCP/plugin discovery so the Rust backend picks up any new tools.
// -------------------------------------------------------------------------------------------------

#[pyfunction]
fn refresh_toolset_cache(py: Python<'_>) -> PyResult<()> {
    init_toolset_cache(py)
}

// -------------------------------------------------------------------------------------------------
// -------------------------------------------------------------------------------------------------
// register_last_resolved_callback — Python calls this to register a callback
// that Rust invokes at the end of get_tool_definitions to update _last_resolved_tool_names
// -------------------------------------------------------------------------------------------------

#[pyfunction]
fn register_last_resolved_callback(_py: Python<'_>, callback: Py<PyAny>) -> PyResult<()> {
    let mut guard = SET_LAST_RESOLVED_CALLBACK.lock().unwrap();
    *guard = Some(callback);
    Ok(())
}

// Query functions
// -------------------------------------------------------------------------------------------------

#[pyfunction]
fn get_tool_to_toolset_map(py: Python<'_>) -> PyResult<Py<PyAny>> {
    let map = TOOL_TO_TOOLSET_MAP.lock().unwrap();
    let dict = PyDict::new(py);
    for (k, v) in map.iter() {
        dict.set_item(k, v)?;
    }
    Ok(dict.into())
}

#[pyfunction]
fn get_toolset_requirements(py: Python<'_>) -> PyResult<Py<PyAny>> {
    let result = registry_obj(py)?
        .bind(py)
        .call_method0("get_toolset_requirements")?;
    Ok(result.into())
}

#[pyfunction]
fn get_all_tool_names(py: Python<'_>) -> PyResult<Py<PyAny>> {
    let mut names: Vec<String> = ALL_TOOLS.lock().unwrap().iter().cloned().collect();
    names.sort();
    Ok(PyList::new(py, &names).unwrap().into())
}

#[pyfunction]
fn get_toolset_for_tool(_py: Python<'_>, tool_name: String) -> PyResult<Option<String>> {
    let map = TOOL_TO_TOOLSET_MAP.lock().unwrap();
    Ok(map.get(&tool_name).cloned())
}

#[pyfunction]
fn get_available_toolsets(py: Python<'_>) -> PyResult<Py<PyAny>> {
    let resolved_map = TOOLSET_RESOLVED.lock().unwrap();
    let dict = PyDict::new(py);
    for (ts_name, tools) in resolved_map.iter() {
        let inner = PyDict::new(py);
        inner.set_item("available", true)?;
        inner.set_item("tools", PyList::new(py, &tools.clone())?)?;
        inner.set_item("description", "")?;
        inner.set_item("requirements", PyList::new(py, &[] as &[String]).unwrap())?;
        dict.set_item(ts_name, inner)?;
    }
    Ok(dict.into())
}

#[pyfunction]
fn check_toolset_requirements(py: Python<'_>) -> PyResult<Py<PyAny>> {
    let result = registry_obj(py)?
        .bind(py)
        .call_method0("check_toolset_requirements")?;
    Ok(result.into())
}

#[pyfunction(signature = (quiet=false))]
fn check_tool_availability(py: Python<'_>, quiet: bool) -> PyResult<Py<PyAny>> {
    let kwargs = PyDict::new(py);
    kwargs.set_item("quiet", quiet)?;
    let result =
        registry_obj(py)?
            .bind(py)
            .call_method("check_toolset_availability", (), Some(&kwargs))?;
    Ok(result.into())
}

// -------------------------------------------------------------------------------------------------
// Module definition
// -------------------------------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Message sanitization — runs before every LLM call
// ---------------------------------------------------------------------------

/// Parse messages from JSON string, remove orphaned tool results, inject stubs.
/// Input: JSON string of message list.
/// Returns: None if no changes needed, Some(json_string) with modified messages.
pub fn sanitize_api_messages_json(json_str: &str) -> Option<String> {
    use serde_json::Value;

    let messages: Vec<Value> = match serde_json::from_str(json_str) {
        Ok(v) => v,
        Err(_) => return None,
    };

    // Collect call IDs from assistant messages
    let mut surviving: std::collections::HashSet<String> = std::collections::HashSet::new();
    for msg in &messages {
        if msg.get("role").and_then(|r| r.as_str()) != Some("assistant") {
            continue;
        }
        let Some(arr) = msg.get("tool_calls").and_then(|v| v.as_array()) else { continue };
        for tc in arr {
            if let Some(id_val) = tc.get("id") {
                if let Some(s) = id_val.as_str() {
                    surviving.insert(s.to_string());
                    continue;
                }
            }
            // fallback: function.name style {function: {name, arguments, id}}
            if let Some(f) = tc.get("function") {
                if let Some(id_val) = f.get("id") {
                    if let Some(s) = id_val.as_str() {
                        surviving.insert(s.to_string());
                    }
                }
            }
        }
    }

    // Collect result IDs from tool messages
    let mut result_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
    for msg in &messages {
        if msg.get("role").and_then(|r| r.as_str()) != Some("tool") {
            continue;
        }
        if let Some(id_val) = msg.get("tool_call_id") {
            if let Some(s) = id_val.as_str() {
                result_ids.insert(s.to_string());
            }
        }
    }

    let orphaned: Vec<String> = result_ids.difference(&surviving).cloned().collect();
    let missing: Vec<String> = surviving.difference(&result_ids).cloned().collect();

    if orphaned.is_empty() && missing.is_empty() {
        return None;
    }

    let mut result: Vec<Value> = Vec::with_capacity(messages.len() + missing.len());
    for msg in &messages {
        let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("");
        if role == "tool" {
            let cid = msg.get("tool_call_id").and_then(|id| id.as_str()).unwrap_or("");
            if orphaned.iter().any(|s| s == cid) {
                continue;
            }
        }
        result.push(msg.clone());
        if role == "assistant" {
            let Some(arr) = msg.get("tool_calls").and_then(|v| v.as_array()) else { continue };
            for tc in arr {
                let cid = tc.get("id").and_then(|id| id.as_str())
                    .or_else(|| {
                        let f = tc.get("function")?;
                        let id_val = f.get("id")?;
                        id_val.as_str()
                    })
                    .unwrap_or("");
                if missing.iter().any(|s| s == cid) {
                    result.push(serde_json::json!({
                        "role": "tool",
                        "content": "[Result unavailable — see context summary above]",
                        "tool_call_id": cid,
                    }));
                }
            }
        }
    }
    serde_json::to_string(&result).ok()
}

#[pyfunction]
fn sanitize_api_messages(messages_json: &str) -> PyResult<Option<String>> {
    Ok(sanitize_api_messages_json(messages_json))
}

#[pymodule]
fn _model_tools_rust(py: Python<'_>, module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_function(wrap_pyfunction!(initialize, module)?)?;
    module.add_function(wrap_pyfunction!(get_tool_definitions, module)?)?;
    module.add_function(wrap_pyfunction!(handle_function_call, module)?)?;
    module.add_function(wrap_pyfunction!(get_tool_to_toolset_map, module)?)?;
    module.add_function(wrap_pyfunction!(get_toolset_requirements, module)?)?;
    module.add_function(wrap_pyfunction!(get_all_tool_names, module)?)?;
    module.add_function(wrap_pyfunction!(get_toolset_for_tool, module)?)?;
    module.add_function(wrap_pyfunction!(get_available_toolsets, module)?)?;
    module.add_function(wrap_pyfunction!(check_toolset_requirements, module)?)?;
    module.add_function(wrap_pyfunction!(check_tool_availability, module)?)?;
    module.add_function(wrap_pyfunction!(sanitize_api_messages, module)?)?;
    module.add_function(wrap_pyfunction!(refresh_toolset_cache, module)?)?;
    module.add_function(wrap_pyfunction!(register_last_resolved_callback, module)?)?;
    module.add(
        "__doc__",
        "Rust backend for Hermes model_tools orchestration.",
    )?;
    if py.version_info().major < 3 {
        return Err(PyException::new_err("Python 3 is required"));
    }
    Ok(())
}
