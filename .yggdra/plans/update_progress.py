#!/usr/bin/env python3
"""
Plan progress tracker utility.
Queries SQL database and updates active-plan.md with current completion status.
Usage: python3 .yggdra/plans/update_progress.py
"""

import sqlite3
import subprocess
from datetime import datetime
from pathlib import Path

def get_plan_progress(db_path):
    """Query database for plan progress."""
    conn = sqlite3.connect(db_path)
    conn.row_factory = sqlite3.Row
    cursor = conn.cursor()
    
    cursor.execute("""
        SELECT 
          f.id as feature_id,
          f.title as feature_title,
          COUNT(t.id) as total_todos,
          SUM(CASE WHEN t.status = 'done' THEN 1 ELSE 0 END) as done_count,
          SUM(CASE WHEN t.status = 'in_progress' THEN 1 ELSE 0 END) as in_progress_count,
          SUM(CASE WHEN t.status = 'pending' THEN 1 ELSE 0 END) as pending_count
        FROM features f
        LEFT JOIN todos t ON f.id = t.feature_id
        WHERE f.plan_id = 'tool-calling-features'
        GROUP BY f.id, f.title
        ORDER BY 
          CASE f.priority 
            WHEN 'critical' THEN 0 
            WHEN 'high' THEN 1 
            WHEN 'medium' THEN 2 
            ELSE 3 
          END,
          f.id
    """)
    
    features = [dict(row) for row in cursor.fetchall()]
    
    # Get overall stats
    cursor.execute("""
        SELECT 
          COUNT(*) as total,
          SUM(CASE WHEN status = 'done' THEN 1 ELSE 0 END) as done,
          SUM(CASE WHEN status = 'in_progress' THEN 1 ELSE 0 END) as in_progress
        FROM todos WHERE feature_id IS NOT NULL
    """)
    overall = dict(cursor.fetchone())
    
    conn.close()
    return features, overall

def format_progress_bar(done, total, width=10):
    """Format a progress bar."""
    if total == 0:
        pct = 0
    else:
        pct = (done / total) * 100
    filled = int((done / total) * width) if total > 0 else 0
    bar = "█" * filled + "░" * (width - filled)
    return f"{bar} {int(pct)}%", int(pct)

def format_status_emoji(done, total, in_progress):
    """Return emoji + status text."""
    if total == 0:
        return "⏳ PENDING"
    if done == total:
        return "✅ DONE"
    if in_progress > 0:
        return "🔄 IN_PROGRESS"
    return "⏳ PENDING"

def update_active_plan(features, overall):
    """Update active-plan.md with current progress."""
    plan_file = Path(__file__).parent / "active-plan.md"
    
    # Build feature list
    feature_lines = []
    for f in features:
        done = f['done_count']
        total = f['total_todos']
        status_emoji = format_status_emoji(done, total, f['in_progress_count'])
        bar, pct = format_progress_bar(done, total)
        
        feature_lines.append(f"**{f['feature_title']}:** {done}/{total} {bar} — {status_emoji}")
    
    # Compute overall
    overall_done = overall['done']
    overall_total = overall['total']
    overall_in_progress = overall['in_progress']
    overall_emoji = format_status_emoji(overall_done, overall_total, overall_in_progress)
    overall_bar, overall_pct = format_progress_bar(overall_done, overall_total)
    
    # Update header
    if plan_file.exists():
        content = plan_file.read_text()
        # Update status line
        content = content.replace(
            f"**Status:** 🔄 IN_PROGRESS",
            f"**Status:** {overall_emoji}"
        )
        # Update progress line
        content = content.replace(
            f"**Progress:** 0/11 todos complete",
            f"**Progress:** {overall_done}/{overall_total} todos complete ({overall_pct}%)"
        )
        plan_file.write_text(content)

if __name__ == "__main__":
    # Find session DB
    copilot_dir = Path.home() / ".copilot" / "session-state"
    for session_dir in copilot_dir.glob("*"):
        session_db = session_dir / "session.db"
        if session_db.exists():
            features, overall = get_plan_progress(str(session_db))
            update_active_plan(features, overall)
            break
