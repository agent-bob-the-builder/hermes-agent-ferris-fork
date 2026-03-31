#!/usr/bin/env python3
"""
Model Tools Module

Thin orchestration layer over the tool registry. Each tool file in tools/
self-registers its schema, handler, and metadata via tools.registry.register().
This module triggers discovery (by importing all tool modules), then provides
the public API that run_agent.py, cli.py, batch_runner.py, and the RL
environments consume.

Public API (signatures preserved from the original 2,400-line version):
    get_tool_definitions(enabled_toolsets, disabled_toolsets, quiet_mode) -> list
    handle_function_call(function_name, function_args, task_id, user_task) -> str
    TOOL_TO_TOOLSET_MAP: dict          (for batch_runner.py)
    TOOLSET_REQUIREMENTS: dict         (for cli.py, doctor.py)
    get_all_tool_names() -> list
    get_toolset_for_tool(name) -> str
    get_available_toolsets() -> dict
    check_toolset_requirements() -> dict
    check_tool_availability(quiet) -> tuple
"""

import os
import json
import asyncio
import logging
import threading
from typing import Dict, Any, List, Optional, Tuple

from tools.registry import registry
from toolsets import resolve_toolset, validate_toolset

# Cached references for hot-path imports — resolved once, reused every call
_cached_notify_fn = None
_cached_invoke_hook = None
_notify_initialized = False
_invoke_hook_initialized = False

logger = logging.getLogger(__name__)


# =============================================================================
# Async Bridging
# =============================================================================

_tool_loop = None
_tool_loop_lock = threading.Lock()
_worker_thread_local = threading.local()


def _get_tool_loop():
    global _tool_loop
    with _tool_loop_lock:
        if _tool_loop is None or _tool_loop.is_closed():
            _tool_loop = asyncio.new_event_loop()
        return _tool_loop


def _get_worker_loop():
    loop = getattr(_worker_thread_local, "loop", None)
    if loop is None or loop.is_closed():
        loop = asyncio.new_event_loop()
        asyncio.set_event_loop(loop)
        _worker_thread_local.loop = loop
    return loop


def _run_async(coro):
    try:
        loop = asyncio.get_running_loop()
    except RuntimeError:
        loop = None

    if loop and loop.is_running():
        import concurrent.futures

        with concurrent.futures.ThreadPoolExecutor(max_workers=1) as pool:
            future = pool.submit(asyncio.run, coro)
            return future.result(timeout=300)

    if threading.current_thread() is not threading.main_thread():
        worker_loop = _get_worker_loop()
        return worker_loop.run_until_complete(coro)

    tool_loop = _get_tool_loop()
    return tool_loop.run_until_complete(coro)


# =============================================================================
# Lazy Tool Discovery
#
# Tool modules are imported on-demand rather than at module load time.
# MCP (~500ms), browser, RL, and other heavy tools are only loaded when needed.
# The registry is populated incrementally; all tools are guaranteed loaded
# before get_tool_definitions() returns.
# =============================================================================

_TOOL_MODULES = [
    "tools.web_tools",
    "tools.terminal_tool",
    "tools.file_tools",
    "tools.vision_tools",
    "tools.mixture_of_agents_tool",
    "tools.image_generation_tool",
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
]

_DISCOVERED_MODULES: set = set()
_DISCOVERY_LOCK = threading.Lock()

_MCP_DISCOVERED = False
_PLUGIN_DISCOVERED = False


def _ensure_tool_module_loaded(mod_name: str) -> bool:
    if mod_name in _DISCOVERED_MODULES:
        return True
    with _DISCOVERY_LOCK:
        if mod_name in _DISCOVERED_MODULES:
            return True
        try:
            import importlib

            importlib.import_module(mod_name)
            _DISCOVERED_MODULES.add(mod_name)
            return True
        except Exception as e:
            logger.warning("Could not import tool module %s: %s", mod_name, e)
            _DISCOVERED_MODULES.add(mod_name)
            return False


def _ensure_all_tools_discovered():
    for mod_name in _TOOL_MODULES:
        _ensure_tool_module_loaded(mod_name)


# Tool name -> module name mapping for scoped discovery
_TOOL_MODULE_MAP = {
    "web_search": "tools.web_tools",
    "web_extract": "tools.web_tools",
    "terminal": "tools.terminal_tool",
    "process": "tools.process_registry",
    "read_file": "tools.file_tools",
    "write_file": "tools.file_tools",
    "patch": "tools.file_tools",
    "search_files": "tools.file_tools",
    "vision_analyze": "tools.vision_tools",
    "image_generate": "tools.image_generation_tool",
    "mixture_of_agents": "tools.mixture_of_agents_tool",
    "skills_list": "tools.skills_tool",
    "skill_view": "tools.skills_tool",
    "skill_manage": "tools.skill_manager_tool",
    "browser_navigate": "tools.browser_tool",
    "browser_snapshot": "tools.browser_tool",
    "browser_click": "tools.browser_tool",
    "browser_type": "tools.browser_tool",
    "browser_scroll": "tools.browser_tool",
    "browser_back": "tools.browser_tool",
    "browser_press": "tools.browser_tool",
    "browser_close": "tools.browser_tool",
    "browser_get_images": "tools.browser_tool",
    "browser_vision": "tools.browser_tool",
    "browser_console": "tools.browser_tool",
    "cronjob": "tools.cronjob_tools",
    "text_to_speech": "tools.tts_tool",
    "todo": "tools.todo_tool",
    "memory": "tools.memory_tool",
    "session_search": "tools.session_search_tool",
    "clarify": "tools.clarify_tool",
    "execute_code": "tools.code_execution_tool",
    "delegate_task": "tools.delegate_tool",
    "send_message": "tools.send_message_tool",
    "honcho_context": "tools.honcho_tools",
    "honcho_profile": "tools.honcho_tools",
    "honcho_search": "tools.honcho_tools",
    "honcho_conclude": "tools.honcho_tools",
    "ha_list_entities": "tools.homeassistant_tool",
    "ha_get_state": "tools.homeassistant_tool",
    "ha_list_services": "tools.homeassistant_tool",
    "ha_call_service": "tools.homeassistant_tool",
    "rl_list_environments": "tools.rl_training_tool",
    "rl_select_environment": "tools.rl_training_tool",
    "rl_get_current_config": "tools.rl_training_tool",
    "rl_edit_config": "tools.rl_training_tool",
    "rl_start_training": "tools.rl_training_tool",
    "rl_check_status": "tools.rl_training_tool",
    "rl_stop_training": "tools.rl_training_tool",
    "rl_get_results": "tools.rl_training_tool",
    "rl_list_runs": "tools.rl_training_tool",
    "rl_test_inference": "tools.rl_training_tool",
}


def _ensure_tools_for_toolset(toolset_name: str):
    from toolsets import resolve_toolset

    try:
        tools = resolve_toolset(toolset_name)
    except Exception:
        _ensure_all_tools_discovered()
        # Refresh Rust cache so newly-loaded tools are available via rs_dispatch
        rust = _ensure_rust_backend()
        if rust:
            try:
                rust.refresh_toolset_cache()
            except Exception:
                pass
        return

    needed = {_TOOL_MODULE_MAP[t] for t in tools if t in _TOOL_MODULE_MAP}
    for mod_name in needed:
        _ensure_tool_module_loaded(mod_name)


def _ensure_tool_for_dispatch(tool_name: str):
    mod_name = _TOOL_MODULE_MAP.get(tool_name)
    if mod_name:
        _ensure_tool_module_loaded(mod_name)
    else:
        _ensure_all_tools_discovered()
    # Refresh Rust cache so newly-loaded tools are available via rs_dispatch
    rust = _ensure_rust_backend()
    if rust:
        try:
            rust.refresh_toolset_cache()
        except Exception:
            pass


def _ensure_mcp_discovered():
    global _MCP_DISCOVERED
    if _MCP_DISCOVERED:
        return
    _MCP_DISCOVERED = True
    try:
        from tools.mcp_tool import discover_mcp_tools

        discover_mcp_tools()

        # After MCP tools are registered, refresh the Rust toolset cache
        # so the backend sees the newly added MCP tools.
        rust = _ensure_rust_backend()
        if rust:
            try:
                rust.refresh_toolset_cache()
            except Exception:
                pass  # Stale .so, ignore
    except Exception as e:
        logger.debug("MCP tool discovery failed: %s", e)


def _ensure_plugins_discovered():
    global _PLUGIN_DISCOVERED
    if _PLUGIN_DISCOVERED:
        return
    _PLUGIN_DISCOVERED = True
    try:
        from hermes_cli.plugins import discover_plugins

        discover_plugins()

        # After plugins are registered, refresh the Rust toolset cache
        # so the backend sees any newly added plugin tools.
        rust = _ensure_rust_backend()
        if rust:
            try:
                rust.refresh_toolset_cache()
            except Exception:
                pass  # Stale .so, ignore
    except Exception as e:
        logger.debug("Plugin discovery failed: %s", e)


# =============================================================================
# Rust backend — deferred until first get_tool_definitions
#
# _rust.initialize() requires the Python tool registry to be populated first.
# We defer both the Rust import AND init to the first get_tool_definitions()
# call, after Python-side tool discovery is complete.
# _rust takes three states: None="not tried yet", False="tried/failed", object="ok"
# =============================================================================

_rust = None  # None = not yet tried; False = tried/failed; object = OK
_use_rust = False  # True when Rust backend is available and initialized


def _ensure_rust_backend():
    global _rust
    # HERMES_USE_RUST=0 disables the Rust backend entirely — used by benchmarks
    # to measure pure Python fallback performance without Rust interference.
    if os.environ.get("HERMES_USE_RUST", "1") == "0":
        _rust = False
        return False
    if _rust is not None:
        return _rust or False
    try:
        import _model_tools_rs as _rust_mod

        _rust_mod.initialize()

        def _set_last_resolved(names: List[str]) -> None:
            global _last_resolved_tool_names
            _last_resolved_tool_names = names

        _rust_mod.register_last_resolved_callback(_set_last_resolved)
        _rust = _rust_mod
        global _use_rust
        _use_rust = True
        logger.debug("model_tools: Rust backend initialized OK")
    except Exception as e:
        logger.debug("model_tools: Rust backend init failed (%s), using Python", e)
        _rust = False
    return _rust


# =============================================================================
# Module-level lazy constants (populated on first access, cached thereafter)
# Exported for backward compatibility with code that imports them directly.
# =============================================================================

_TOOL_TO_TOOLSET_MAP: Optional[Dict[str, str]] = None
_TOOLSET_REQUIREMENTS: Optional[Dict[str, dict]] = None


def _ensure_constants():
    global _TOOL_TO_TOOLSET_MAP, _TOOLSET_REQUIREMENTS, _USING_RUST_BACKEND
    if _TOOL_TO_TOOLSET_MAP is not None:
        return
    # Ensure all tool modules are loaded and have called registry.register()
    # so that registry._tools is populated before we try to build the map.
    _ensure_all_tools_discovered()
    rust = _ensure_rust_backend()
    if rust:
        _TOOL_TO_TOOLSET_MAP = rust.get_tool_to_toolset_map()
        _TOOLSET_REQUIREMENTS = rust.get_toolset_requirements()
        _USING_RUST_BACKEND = True
    else:
        _TOOL_TO_TOOLSET_MAP = registry.get_tool_to_toolset_map()
        _TOOLSET_REQUIREMENTS = registry.get_toolset_requirements()
        _USING_RUST_BACKEND = False


# Module-level __getattr__ for lazy TOOL_TO_TOOLSET_MAP / TOOLSET_REQUIREMENTS
def __getattr__(name: str):
    if name in ("TOOL_TO_TOOLSET_MAP", "TOOLSET_REQUIREMENTS"):
        _ensure_constants()
        return globals()[f"_{name}"]
    raise AttributeError(f"module {__name__!r} has no attribute {name!r}")


_USING_RUST_BACKEND: bool = False
_last_resolved_tool_names: List[str] = []

# Cache for resolved toolset -> tool names
_toolset_resolve_cache: Dict[str, List[str]] = {}

# Pre-compiled regex for fast toolset name validation (alphanumeric + underscore, starts with letter)
_TOOLSET_NAME_RE = __import__("re").compile(r"^[a-zA-Z][a-zA-Z0-9_]*$")


def _is_valid_toolset_name(name: str) -> bool:
    """Fast pre-compiled check if a toolset name looks valid before calling validate_toolset."""
    return bool(_TOOLSET_NAME_RE.match(name))


def _cached_resolve_toolset(name: str) -> List[str]:
    if name not in _toolset_resolve_cache:
        _toolset_resolve_cache[name] = resolve_toolset(name)
    return _toolset_resolve_cache[name]


# Module-level cache for get_tool_definitions
_get_definitions_cache: Dict[tuple, List[Dict[str, Any]]] = {}


# =============================================================================
# Legacy toolset name mapping
# =============================================================================

_LEGACY_TOOLSET_MAP = {
    "web_tools": ["web_search", "web_extract"],
    "terminal_tools": ["terminal"],
    "vision_tools": ["vision_analyze"],
    "moa_tools": ["mixture_of_agents"],
    "image_tools": ["image_generate"],
    "skills_tools": ["skills_list", "skill_view", "skill_manage"],
    "browser_tools": [
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
    "cronjob_tools": ["cronjob"],
    "rl_tools": [
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
    "file_tools": ["read_file", "write_file", "patch", "search_files"],
    "tts_tools": ["text_to_speech"],
}


# =============================================================================
# get_tool_definitions
# =============================================================================


def get_tool_definitions(
    enabled_toolsets: List[str] = None,
    disabled_toolsets: List[str] = None,
    quiet_mode: bool = False,
) -> List[Dict[str, Any]]:
    """
    Get tool definitions for model API calls with toolset-based filtering.

    All tools must be part of a toolset to be accessible.

    Args:
        enabled_toolsets: Only include tools from these toolsets.
        disabled_toolsets: Exclude tools from these toolsets (if enabled_toolsets is None).
        quiet_mode: Suppress status prints.

    Returns:
        Filtered list of OpenAI-format tool definitions.
    """
    _ensure_mcp_discovered()
    _ensure_plugins_discovered()

    rust = _ensure_rust_backend()
    _ensure_constants()

    _cache_key = (
        tuple(sorted(enabled_toolsets) if enabled_toolsets else ()),
        tuple(sorted(disabled_toolsets) if disabled_toolsets else ()),
        quiet_mode,
    )
    if _cache_key in _get_definitions_cache:
        return _get_definitions_cache[_cache_key]

    # Ensure all Python tools are discovered before Rust sees them
    _ensure_all_tools_discovered()

    if rust:
        try:
            result = rust.get_tool_definitions(
                enabled_toolsets=enabled_toolsets,
                disabled_toolsets=disabled_toolsets,
                quiet_mode=quiet_mode,
            )
            return result
        except Exception as e:
            logger.warning(
                "Rust get_tool_definitions failed: %s, falling back to Python", e
            )

    # Python fallback
    # Build tools_to_include in one pass using list + extend (faster than repeated update)
    tools_to_include_list: List[str] = []

    if enabled_toolsets is not None:
        for toolset_name in enabled_toolsets:
            if _is_valid_toolset_name(toolset_name) and validate_toolset(toolset_name):
                resolved = _cached_resolve_toolset(toolset_name)
                tools_to_include_list.extend(resolved)
                if not quiet_mode:
                    print(
                        f"✅ Enabled toolset '{toolset_name}': {', '.join(resolved) if resolved else 'no tools'}"
                    )
            elif toolset_name in _LEGACY_TOOLSET_MAP:
                legacy_tools = _LEGACY_TOOLSET_MAP[toolset_name]
                tools_to_include_list.extend(legacy_tools)
                if not quiet_mode:
                    print(
                        f"✅ Enabled legacy toolset '{toolset_name}': {', '.join(legacy_tools)}"
                    )
            else:
                if not quiet_mode:
                    print(f"⚠️  Unknown toolset: {toolset_name}")
        tools_to_include = set(tools_to_include_list)

    elif disabled_toolsets:
        from toolsets import get_all_toolsets

        for ts_name in get_all_toolsets():
            tools_to_include_list.extend(_cached_resolve_toolset(ts_name))
        tools_to_include = set(tools_to_include_list)

        for toolset_name in disabled_toolsets:
            if _is_valid_toolset_name(toolset_name) and validate_toolset(toolset_name):
                resolved = _cached_resolve_toolset(toolset_name)
                tools_to_include.difference_update(resolved)
                if not quiet_mode:
                    print(
                        f"🚫 Disabled toolset '{toolset_name}': {', '.join(resolved) if resolved else 'no tools'}"
                    )
            elif toolset_name in _LEGACY_TOOLSET_MAP:
                legacy_tools = _LEGACY_TOOLSET_MAP[toolset_name]
                tools_to_include.difference_update(legacy_tools)
                if not quiet_mode:
                    print(
                        f"🚫 Disabled legacy toolset '{toolset_name}': {', '.join(legacy_tools)}"
                    )
            else:
                if not quiet_mode:
                    print(f"⚠️  Unknown toolset: {toolset_name}")
    else:
        from toolsets import get_all_toolsets

        for ts_name in get_all_toolsets():
            tools_to_include_list.extend(_cached_resolve_toolset(ts_name))
        tools_to_include = set(tools_to_include_list)

    filtered_tools = registry.get_definitions(tools_to_include, quiet=quiet_mode)

    # Pre-compute tool name set once
    available_tool_names = {t["function"]["name"] for t in filtered_tools}
    # Build index dict for O(1) lookup instead of O(n) list iteration
    tool_index: Dict[str, int] = {
        td["function"]["name"]: i for i, td in enumerate(filtered_tools)
    }

    if "execute_code" in available_tool_names:
        from tools.code_execution_tool import (
            SANDBOX_ALLOWED_TOOLS,
            build_execute_code_schema,
        )

        sandbox_enabled = SANDBOX_ALLOWED_TOOLS & available_tool_names
        dynamic_schema = build_execute_code_schema(sandbox_enabled)
        idx = tool_index.get("execute_code")
        if idx is not None:
            filtered_tools[idx] = {"type": "function", "function": dynamic_schema}

    if "browser_navigate" in available_tool_names:
        web_tools_available = {"web_search", "web_extract"} & available_tool_names
        if not web_tools_available:
            idx = tool_index.get("browser_navigate")
            if idx is not None:
                td = filtered_tools[idx]
                desc = td["function"].get("description", "")
                desc = desc.replace(
                    " For simple information retrieval, prefer web_search or web_extract (faster, cheaper).",
                    "",
                )
                filtered_tools[idx] = {
                    "type": "function",
                    "function": {**td["function"], "description": desc},
                }

    global _last_resolved_tool_names
    if not quiet_mode:
        if filtered_tools:
            tool_names = [t["function"]["name"] for t in filtered_tools]
            print(
                f"🛠️  Final tool selection ({len(filtered_tools)} tools): {', '.join(tool_names)}"
            )
        else:
            print("🛠️  No tools selected (all filtered out or unavailable)")

    _last_resolved_tool_names = [t["function"]["name"] for t in filtered_tools]

    _get_definitions_cache[_cache_key] = filtered_tools
    return filtered_tools


# =============================================================================
# handle_function_call
# =============================================================================

_AGENT_LOOP_TOOLS = {"todo", "memory", "session_search", "delegate_task"}
_READ_SEARCH_TOOLS = {"read_file", "search_files"}


def handle_function_call(
    function_name: str,
    function_args: Dict[str, Any],
    task_id: Optional[str] = None,
    user_task: Optional[str] = None,
    enabled_tools: Optional[List[str]] = None,
    honcho_manager: Optional[Any] = None,
    honcho_session_key: Optional[str] = None,
) -> str:
    """
    Main function call dispatcher that routes calls to the tool registry.

    Args:
        function_name: Name of the function to call.
        function_args: Arguments for the function.
        task_id: Unique identifier for terminal/browser session isolation.
        user_task: The user's original task (for browser_snapshot context).
        enabled_tools: Tool names enabled for this session.
        honcho_manager: Honcho manager instance for Honcho-tool calls.
        honcho_session_key: Session key for Honcho-tool calls.

    Returns:
        Function result as a JSON string.
    """
    # Lazy discovery: only load the specific tool module we need
    _ensure_tool_for_dispatch(function_name)

    rust = _ensure_rust_backend()

    # Try Rust rs_dispatch first — O(1) HashMap lookup + direct handler call,
    # no re-entrance into Python registry.dispatch() so no deadlock risk.
    # Falls back to Python dispatch for unknown tools and async handlers.
    if rust and hasattr(rust, "rs_dispatch"):
        try:
            result = rust.rs_dispatch(
                function_name=function_name,
                function_args=function_args,
                task_id=task_id,
                user_task=user_task,
                enabled_tools=enabled_tools,
                last_resolved_tool_names=_last_resolved_tool_names,
                honcho_manager=honcho_manager,
                honcho_session_key=honcho_session_key,
            )
            if result is not None:
                return result
        except Exception as e:
            logger.warning(
                "Rust rs_dispatch failed: %s, falling back to Python", e
            )

    # Python fallback
    if function_name not in _READ_SEARCH_TOOLS:
        global _cached_notify_fn, _notify_initialized
        if not _notify_initialized:
            try:
                from tools.file_tools import notify_other_tool_call

                _cached_notify_fn = notify_other_tool_call
            except Exception:
                pass
            _notify_initialized = True
        if _cached_notify_fn is not None:
            try:
                _cached_notify_fn(task_id or "default")
            except Exception:
                pass

    try:
        if function_name in _AGENT_LOOP_TOOLS:
            return json.dumps(
                {"error": f"{function_name} must be handled by the agent loop"}
            )

        global _cached_invoke_hook, _invoke_hook_initialized
        if not _invoke_hook_initialized:
            try:
                from hermes_cli.plugins import invoke_hook

                _cached_invoke_hook = invoke_hook
            except Exception:
                pass
            _invoke_hook_initialized = True
        if _cached_invoke_hook is not None:
            try:
                _cached_invoke_hook(
                    "pre_tool_call",
                    tool_name=function_name,
                    args=function_args,
                    task_id=task_id or "",
                )
            except Exception:
                pass

        # Guard _invoke_hook post-call with non-blocking wrap to prevent
        # any plugin hook from blocking the dispatch result.
        _post_hook = _cached_invoke_hook
        _post_hook_registered = _invoke_hook_initialized

        if function_name == "execute_code":
            sandbox_enabled = (
                enabled_tools
                if enabled_tools is not None
                else _last_resolved_tool_names
            )
            result = registry.dispatch(
                function_name,
                function_args,
                task_id=task_id,
                enabled_tools=sandbox_enabled,
                honcho_manager=honcho_manager,
                honcho_session_key=honcho_session_key,
            )
        else:
            result = registry.dispatch(
                function_name,
                function_args,
                task_id=task_id,
                user_task=user_task,
                honcho_manager=honcho_manager,
                honcho_session_key=honcho_session_key,
            )

        if _post_hook is not None and _post_hook_registered:
            try:
                # Non-blocking post-hook: fire and forget. Never let plugin
                # code block the tool result from reaching the caller.
                import threading
                def _fire_and_forget():
                    try:
                        _post_hook(
                            "post_tool_call",
                            tool_name=function_name,
                            args=function_args,
                            result=result,
                            task_id=task_id or "",
                        )
                    except Exception:
                        pass
                threading.Thread(target=_fire_and_forget, daemon=True).start()
            except Exception:
                pass

        return result

    except Exception as e:
        error_msg = f"Error executing {function_name}: {str(e)}"
        logger.error(error_msg)
        return json.dumps({"error": error_msg}, ensure_ascii=False)


# =============================================================================
# Backward-compat wrapper functions
# =============================================================================


def get_all_tool_names() -> List[str]:
    _ensure_all_tools_discovered()
    return registry.get_all_tool_names()


def get_toolset_for_tool(tool_name: str) -> Optional[str]:
    _ensure_constants()
    return _TOOL_TO_TOOLSET_MAP.get(tool_name)


def get_available_toolsets() -> Dict[str, dict]:
    _ensure_constants()
    return registry.get_available_toolsets()


def check_toolset_requirements() -> Dict[str, bool]:
    _ensure_constants()
    return registry.check_toolset_requirements()


def check_tool_availability(quiet: bool = False) -> Tuple[List[str], List[dict]]:
    return registry.check_tool_availability(quiet=quiet)
