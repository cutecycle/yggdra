# Task-Aware Notification Implementation

## Overview

yggdra now sends notifications with task-aware titles that show what goal was accomplished, along with detailed metrics in the body.

## Format

### Notification Title
```
yggdra: [user's goal/task summary]
```

Examples:
- `yggdra: fix the bug in main.rs`
- `yggdra: analyze performance metrics`
- `yggdra: add token tracking feature`

### Notification Body
```
Completed in One mode.

Tokens used: X
Response length: Y characters
```

## Implementation Details

### Title Generation Algorithm

1. **Extract goal** from `app.input` (user's typed task)
2. **Handle multiline**: Take first line if input contains `\n`
3. **Truncate long**: Limit to ~50 characters + "..."
4. **Prefix**: Add "yggdra: " prefix
5. **Result**: `yggdra: [goal]`

### Code Location

- **Function**: `send_task_completion(goal: &str, tokens: u32, response_len: usize)`
- **File**: `src/notifications.rs`
- **Called from**: `src/main.rs` (One mode completion handler)

### Signature
```rust
pub fn send_task_completion(goal: &str, tokens_used: u32, response_length: usize)
```

## Examples

### Example 1: Short Goal
```
User input:  "get weather data"
Notification title:  "yggdra: get weather data"
Notification body:   "Completed in One mode.\n\nTokens used: 256\nResponse length: 1024 characters"
```

### Example 2: Long Goal (Truncated)
```
User input:  "analyze the entire codebase and suggest optimizations for performance"
Notification title:  "yggdra: analyze the entire codebase and sug..."
Notification body:   "Completed in One mode.\n\nTokens used: 1024\nResponse length: 5120 characters"
```

### Example 3: Multiline Goal (First Line Used)
```
User input:  "Build feature\nDetails: Add token counter"
Notification title:  "yggdra: Build feature"
Notification body:   "Completed in One mode.\n\nTokens used: 512\nResponse length: 2048 characters"
```

### Example 4: Empty Goal (Fallback)
```
User input:  "" (empty)
Notification title:  "yggdra: Task complete"
Notification body:   "Completed in One mode.\n\nTokens used: 128\nResponse length: 512 characters"
```

## How It Works

### Workflow

1. User types goal in input field: `"fix startup bug"`
2. User presses Enter or types `/one` command
3. One mode activates and agent begins processing
4. Agent completes task and outputs `[DONE]`
5. Application detects completion
6. **Notification sent:**
   - Title: `"yggdra: fix startup bug"`
   - Body: `"Completed in One mode.\n\nTokens used: 512\nResponse length: 2048 characters"`
7. User sees notification in notification center

### Automatic Processing

- **No configuration needed**: Works out of the box
- **Automatic extraction**: Goal comes from `app.input`
- **Smart truncation**: Long goals are truncated intelligently
- **Metrics included**: Token usage and response length shown

## Benefits

✓ **Clear summaries**: Title tells you what was accomplished
✓ **Detailed metrics**: Body shows resource usage
✓ **Non-intrusive**: Notification center, not popup
✓ **Persistent**: Stays in notification history
✓ **Actionable**: Shows both goal and results
✓ **Professional**: Clean, well-formatted output

## Testing

- ✓ All 264 unit tests pass
- ✓ Compiles cleanly with `--release`
- ✓ Installed to `~/.local/bin/yggdra`
- ✓ Ready for production use

## Files Modified

1. **src/notifications.rs**: Updated `send_task_completion()` function
2. **src/main.rs**: Updated One mode completion handler to pass goal

## Integration Points

- **One mode completion** (main.rs line ~245)
- **Notification sending** (notifications.rs)
- **Goal extraction** (app.input field)
- **Metrics** (last message tokens and content length)

## Future Enhancements

Possible improvements:
- Show error summary if task failed
- Include timestamps in body
- Add task status emoji to title
- Support for other notification triggers (not just One mode completion)

