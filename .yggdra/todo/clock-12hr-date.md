# Format clock notification with 12-hour time and full date

**Status:** pending  
**Priority:** medium

## Current behavior
Clock notification shows 24-hour time without date:
```
🕐 14:30
```

## Target behavior
Format: 12-hour time with full date
```
🕐 2:30 PM • Apr 11, 2026
```

## Implementation
**File:** `src/ui.rs` line 827 in `persist_message()`

Current code:
```rust
let timestamp = chrono::Local::now().format("%H:%M").to_string();
```

Update format string to:
```rust
let timestamp = chrono::Local::now().format("%I:%M %p • %b %d, %Y").to_string();
```

Or with leading zero trim for 12-hour format:
```rust
let timestamp = chrono::Local::now().format("%-I:%M %p • %b %d, %Y").to_string();
```

## Notes
- Use `chrono` crate (already in Cargo.toml)
- Format codes: `%I` = 12-hour, `%M` = minutes, `%p` = AM/PM, `%b` = month abbrev, `%d` = day, `%Y` = year
- `%-I` removes leading zero from hour if needed
