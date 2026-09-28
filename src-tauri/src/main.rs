// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // Claude Code runs this binary as its status line to report plan usage.
    if std::env::args().nth(1).as_deref() == Some(claude_legend_lib::STATUSLINE_RELAY_ARG) {
        return claude_legend_lib::statusline_relay();
    }
    // Claude Code runs it as a hook to report what sessions are waiting for.
    if std::env::args().nth(1).as_deref() == Some(claude_legend_lib::HOOK_RELAY_ARG) {
        return claude_legend_lib::hook_relay();
    }
    claude_legend_lib::run()
}
