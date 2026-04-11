# Yggdra Todo List

This folder contains actionable todos for the agent. Each file is a standalone task with status, requirements, and implementation hints.

## Format
- **Filename:** kebab-case ID (e.g., `clock-12hr-date.md`)
- **Status:** marked in frontmatter (pending / in_progress / done)
- **Content:** Markdown with clear implementation guidance

## Workflow
1. **Agent discovery:** Use `[TOOL: rg TODO .yggdra/todo/]` or `[TOOL: spawn ls .yggdra/todo/]`
2. **Pick a task:** Read the file with `[TOOL: editfile .yggdra/todo/TASKNAME.md]`
3. **Work on it:** Update status to `in_progress`, implement, test
4. **Complete:** Update status to `done` when finished
5. **Next task:** Find another pending todo and repeat

## Current tasks
- `clock-12hr-date.md` — Format clock with 12-hour time and full date (pending)
