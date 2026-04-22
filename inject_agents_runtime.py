import os

file_path = 'src/agent.rs'
with open(file_path, 'r') as f:
    lines = f.readlines()

# 1. Find the function start
target_line_idx = -1
for i, line in enumerate(lines):
    if 'fn system_prompt_with_steering' in line:
        target_line_idx = i
        break

if target_line_idx == -1:
    print('Could not find function')
    exit(1)

# 2. Insert the runtime loading logic at the start of the function
# This is much safer than embedding the literal text!
runtime_logic = [
    '    let agents_md_path = format!("{}/.yggdra/agents.md", std::env::var("HOME").unwrap_or_default());\n',
    '    let agents_md = std::fs::read_to_string(agents_md_path).unwrap_or_default();\n'
]
for i, line in enumerate(runtime_logic):
    lines.insert(target_line_idx + 1 + i, line)

# 3. Find the format! calls and inject 'agents_md, ' as the first argument
# We scan the function scope (the next 200 lines)
found_count = 0
for i in range(target_line_idx + 1, min(target_line_idx + 200, len(lines))):
    if 'format!(' in lines[i]:
        line = lines[i]
        # Look for delimiters: r#" , " , r#' , '
        delims = [('r#"', 3), ('"', 1), ("r#'", 3), ("'", 1)]
        for d, length in delims:
            idx = line.find(d)
            if idx != -1:
                lines[i] = line[:idx+length] + 'agents_md, ' + line[idx+length:]
                found_count += 1
                break

# 4. Write the changes back
with open(file_path, 'w') as f:
    f.writelines(lines)

print(f'Successfully implemented runtime loading for agents.md. Updated {found_count} format! calls.')
