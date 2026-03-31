"""Rust-native tool registry.

All tool state lives in Rust. This module is a thin shim that exposes the same
API as the old Python ToolRegistry, but delegates to the Rust extension instead.

This file is imported by tools/*.py ONLY via tools/registry.py, which
transparently delegates to this module when HERMES_USE_RUST_MODEL_TOOLS=1.
"""

import json as _json

# Lazy import — don't pay the cost of importing the Rust extension at module
# load time. The Rust extension is only imported when a tool actually needs
# to register itself, which only happens after the Rust backend has been
# initialised (model_tools.py → model_tools_rs.initialize()).
_rust = None


def _get_rust():
    global _rust
    if _rust is None:
        import model_tools_rs as _model_tools_rust_module

        class _RustRegistry:
            def __getattr__(self, name: str):
                return getattr(_model_tools_rust_module, name)

        _rust = _RustRegistry()
    return _rust


def register(
    name: str,
    toolset: str,
    schema: dict,
    handler,
    check_fn=None,
    requires_env: list = None,
    is_async: bool = False,
    description: str = "",
    emoji: str = "",
):
    """Register a tool with the Rust-native registry.

    Falls back gracefully if the Rust backend doesn't support direct tool
    registration (stale .so / build out of sync with source).
    """
    rs = _get_rust()
    if not hasattr(rs, "register_tool"):
        # Rust .so is stale — fall back to Python registry for this tool only.
        # The Python ToolRegistry is already imported as _python_registry
        # when this module is first loaded (see below).
        import tools.registry as _py_reg

        _py_reg.registry.register(
            name=name,
            toolset=toolset,
            schema=schema,
            handler=handler,
            check_fn=check_fn,
            requires_env=requires_env,
            is_async=is_async,
            description=description,
            emoji=emoji,
        )
        return
    rs.register_tool(
        name=name,
        toolset=toolset,
        schema=schema,
        handler=handler,
        check_fn=check_fn,
        requires_env=requires_env or [],
        is_async=is_async,
        description=description or schema.get("description", ""),
        emoji=emoji,
    )
    # Invalidate the get_tool_definitions LRU cache since tool state changed.
    rs.clear_get_def_cache()


def get_definitions(tool_names: set, quiet: bool = False) -> list:
    """Return OpenAI-format tool schemas for the requested tool names."""
    rs = _get_rust()
    return rs.rs_get_definitions(sorted(tool_names), quiet)


def dispatch(name: str, args: dict, **kwargs) -> str:
    """Execute a tool handler by name."""
    rs = _get_rust()
    args_json = _json.dumps(args)
    return rs.rs_dispatch(
        function_name=name,
        args_json=args_json,
        task_id=kwargs.get("task_id"),
        user_task=kwargs.get("user_task"),
        enabled_tools=kwargs.get("enabled_tools"),
        last_resolved_tool_names=kwargs.get("last_resolved_tool_names"),
        honcho_manager=kwargs.get("honcho_manager"),
        honcho_session_key=kwargs.get("honcho_session_key"),
    )


def get_all_tool_names() -> list:
    rs = _get_rust()
    return rs.rs_get_all_tool_names()


def get_toolset_for_tool(name: str):
    rs = _get_rust()
    return rs.rs_get_toolset_for_tool(name)


def get_tool_to_toolset_map() -> dict:
    rs = _get_rust()
    return rs.rs_get_tool_to_toolset_map()


def is_toolset_available(toolset: str) -> bool:
    # Note: this bypasses the Rust per-toolset check cache since we'd need
    # to call check_toolset_requirements to get per-toolset bools.
    # For the hot path (is_toolset_available called during init), prefer
    # check_toolset_requirements() and cache locally in Python.
    rs = _get_rust()
    result = rs.rs_check_tool_availability(quiet=True)
    available, _unavailable = result
    return toolset in available


def check_toolset_requirements() -> dict:
    rs = _get_rust()
    return rs.rs_check_toolset_requirements()


def get_available_toolsets() -> dict:
    # Maps toolset name -> {available, tools, description, requirements}
    rs = _get_rust()
    reqs = rs.rs_check_toolset_requirements()
    all_tools = get_all_tool_names()
    tool_to_ts = get_tool_to_toolset_map()

    result = {}
    for tool_name in all_tools:
        ts = tool_to_ts.get(tool_name)
        if ts is None or ts in result:
            continue
        result[ts] = {
            "available": reqs.get(ts, False),
            "tools": [t for t in all_tools if tool_to_ts.get(t) == ts],
            "description": "",
            "requirements": [],
        }
    return result


def check_tool_availability(quiet: bool = False):
    rs = _get_rust()
    available_list, unavailable_list = rs.rs_check_tool_availability(quiet=quiet)
    return available_list, unavailable_list
