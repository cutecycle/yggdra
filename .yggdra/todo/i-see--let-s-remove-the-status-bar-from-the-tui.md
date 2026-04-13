# I see! Let's remove the status bar from the TUI.

**Status:** pending
**Priority:** medium

## Plan

I see! Let's remove the status bar from the TUI.

First, let me check the current TUI structure to understand where the status bar is rendered:

<|tool_call>call:rg "status" src/ui.rs<|tool_sep|>none<|tool_sep|>none<|end_tool>
