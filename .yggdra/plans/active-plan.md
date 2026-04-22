# Tool Calling & Features Implementation
**Plan ID:** tool-calling-features-2026-04-22  
**Created:** 2026-04-22  
**Status:** 🔄 IN_PROGRESS  
**Progress:** 0/11 todos complete

---

## 📋 Feature Breakdown

### 1. 🐛 Tool Calling Bug Fix
**Status:** ⏳ PENDING (0/3 todos)  
**Priority:** 🔴 Critical (blocks other features)  
**Depends On:** None

**Description:** Fix erroneous `sh: sh:` prefix in shell tool execution. Diagnose source, implement fix, verify with tests.

**Todos:**
- [ ] `diagnose-tool-bug` — Locate where `sh: sh:` prefix is added
- [ ] `fix-tool-bug` — Remove prefix injection, verify shell tool receives correct args
- [ ] `test-tool-bug` — Test shell tools work without prefix

---

### 2. 🔄 Ask Mode Autonomy
**Status:** ⏳ PENDING (0/2 todos)  
**Priority:** 🟡 High  
**Depends On:** Tool Calling Bug Fix (must work before autonomy makes sense)

**Description:** Enable Ask mode to autonomously execute read-only tools until agent signals `[DONE]`. Exploration-oriented operation.

**Todos:**
- [ ] `implement-ask-autonomy` — Modify ask mode loop, skip write tools
- [ ] `test-ask-autonomy` — Test with multi-step exploration queries

---

### 3. ⚡ Real-Time Tool Streaming
**Status:** ⏳ PENDING (0/2 todos)  
**Priority:** 🟡 High  
**Depends On:** Tool Calling Bug Fix

**Description:** Parse & inject tool results immediately after each execution (not batched). Agent sees results promptly.

**Todos:**
- [ ] `implement-realtime-streaming` — Refactor agent loop for immediate injection
- [ ] `test-realtime-streaming` — Verify agent receives results promptly

---

### 4. 📝 Markdown Rendering in TUI
**Status:** ⏳ PENDING (0/3 todos)  
**Priority:** 🟢 Medium  
**Depends On:** None

**Description:** Enhance TUI markdown rendering — syntax highlighting, lists, code blocks, tables.

**Todos:**
- [ ] `implement-markdown-rendering` — Add markdown formatting to UI
- [ ] `test-markdown-rendering` — Verify markdown displays correctly
- [ ] `run-full-tests` — `cargo test --lib` must pass (264 tests)

---

### 5. 📦 Release
**Status:** ⏳ PENDING (0/1 todos)  
**Priority:** 🟡 High  
**Depends On:** All features complete

**Todos:**
- [ ] `make-install` — Install updated binary to PATH

---

## 📊 Completion Summary

```
Tool Calling Bug Fix:        0/3  [░░░░░░░░░░] 0%
Ask Mode Autonomy:           0/2  [░░░░░░░░░░] 0%
Real-Time Streaming:         0/2  [░░░░░░░░░░] 0%
Markdown Rendering:          0/3  [░░░░░░░░░░] 0%
Release:                     0/1  [░░░░░░░░░░] 0%
                            ────
TOTAL:                       0/11 [░░░░░░░░░░] 0%
```

---

## 🔗 Dependencies

```
[Tool Bug Fix] ──┐
                 ├→ [Ask Mode Autonomy]
                 ├→ [Real-Time Streaming]
                 └→ [Markdown Rendering]
                        ↓
                 [Release: make install]
```

---

## 📝 Notes

- All features can start after Tool Bug Fix is diagnosed
- Real-time streaming refactor affects agent loop — test thoroughly
- Markdown rendering is lowest priority but enhances UX
- Final `cargo test --lib` before release

