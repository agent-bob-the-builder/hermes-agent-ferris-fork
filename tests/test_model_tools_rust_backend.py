import importlib.util


def test_prefers_rust_backend_when_extension_installed():
    import model_tools

    if importlib.util.find_spec("_model_tools_rust") is None:
        assert model_tools._USING_RUST_BACKEND is False
        return

    assert model_tools._USING_RUST_BACKEND is True


def test_get_tool_definitions_updates_last_resolved_tool_names():
    import model_tools

    tools = model_tools.get_tool_definitions(enabled_toolsets=["file"], quiet_mode=True)

    assert [tool["function"]["name"] for tool in tools] == model_tools._last_resolved_tool_names
