# Plan Tracking System

This directory stores hierarchical plan tracking with three levels:
1. **Plans** — Overall initiatives (e.g., "Tool Calling & Features")
2. **Features** — Major components (e.g., "Tool Calling Bug Fix", "Ask Mode Autonomy")
3. **Todos** — Individual tasks (e.g., "diagnose-tool-bug", "implement-ask-autonomy")

## File Structure

```
.yggdra/plans/
├── plan-tracking.md          (this file)
├── active-plan.md            (current active plan summary)
├── plans/
│   └── <plan-id>/
│       ├── index.md          (plan overview + completion status)
│       └── features/
│           └── <feature-id>.md    (feature + its todos)
└── completed-plans.log       (archive of finished plans)
```

## Status Indicators

- `⏳ PENDING` — Not started
- `🔄 IN_PROGRESS` — Actively being worked on
- `✅ DONE` — Completed and tested
- `⛔ BLOCKED` — Cannot proceed (reason documented)
- `🔀 PARTIAL` — Some todos done, others pending/in-progress

## Hierarchy Example

```
Plan: Tool Calling & Features Implementation
├─ 📊 Status: IN_PROGRESS (8/11 todos done, 3 in progress)
│
├─ Feature: Tool Calling Bug Fix
│  ├─ 📊 Status: ✅ DONE
│  ├─ diagnose-tool-bug: ✅ DONE
│  ├─ fix-tool-bug: ✅ DONE
│  └─ test-tool-bug: ✅ DONE
│
├─ Feature: Ask Mode Autonomy
│  ├─ 📊 Status: 🔄 IN_PROGRESS (1/2 todos done)
│  ├─ implement-ask-autonomy: 🔄 IN_PROGRESS
│  └─ test-ask-autonomy: ⏳ PENDING
│
├─ Feature: Real-Time Streaming
│  ├─ 📊 Status: 🔄 IN_PROGRESS (1/2 todos done)
│  ├─ implement-realtime-streaming: 🔄 IN_PROGRESS
│  └─ test-realtime-streaming: ⏳ PENDING
│
└─ Feature: Markdown Rendering
   ├─ 📊 Status: ⏳ PENDING (0/3 todos done)
   ├─ implement-markdown-rendering: ⏳ PENDING
   ├─ test-markdown-rendering: ⏳ PENDING
   └─ run-full-tests: ⏳ PENDING
```

## Completion Cascade

- A **Feature** is ✅ DONE when all its todos are ✅ DONE
- A **Plan** is ✅ DONE when all its features are ✅ DONE
- Completion is automatically computed from todo status in SQL

## Auto-Generated Reports

When a plan reaches 100% completion:
1. A summary is appended to `completed-plans.log`
2. Timestamps and feature breakdown included
3. Plan marked as archived

