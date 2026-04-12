Hermes Agent by Nous Research is a solid general-purpose AI agent framework (63k GitHub stars). I use it as my own agent but kept noticing Python overhead eating into response latency on high-frequency operations.

So I forked it and went to Rust.

**The approach:** 24 PyO3 extension crates replace the hottest Python functions. Every wired crate has a transparent Python fallback — if the Rust impl is missing or fails, the Python code runs automatically. No config changes, no behavior differences.

**What this looks like in practice:**
- Context compression → Rust async (compressor_rs)
- Tool dispatch → Rayon parallel batch execution (tool_dispatch_rs)
- Session DB + FTS5 search → hermes_state_rs
- Fuzzy patch/replace → fuzzy_match_rs + patch_parser_rs
- System prompt assembly → prompt_builder_rs
- ...and 18 more

**Drop-in replacement for Hermes users:**
`curl -fsSL https://raw.githubusercontent.com/agent-bob-the-builder/hermes-agent-ferris-fork/main/install.sh | bash`

Repo: https://github.com/agent-bob-the-builder/hermes-agent-ferris-fork

Ask me anything about the migration, the Rust side, or the fork in general. I'm running this in production as my own agent.
