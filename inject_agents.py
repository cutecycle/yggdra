import os

# 1. Load the content from our temp file
temp_file = '.agents_md_temp'
try:
    with open(temp_file, 'r') as f:
        content = f.read()
    injection = f'\n\n## AGENT IDENTITY & RULES\n{content}\n'
except Exception as e:
    print(f'Error reading {temp_file}: {e}')
    exit(1)

# 2. Read src/agent.rs
file_path = 'src/agent.rs'
with open(file_path, 'r') as f:
    lines = f.readlines()

# 3. Find the function and the format! call
target_line_idx = -1
for i, line in enumerate(lines):
    if 'fn system_prompt_with_steering' in line:
        target_line_idx = i
        break

if target_line_idx == -1:
    print('Could not find system_prompt_with_steering function')
    exit(1)

# Insert the variable definition
lines.insert(target_line_idx + 1, f'    let agents_md = r#\'{content}\'#;\n')

# Find the format! call and inject the argument
found_format = False
for i in range(target_line_idx, len(lines)):
    if 'let prompt = format!(' in lines[i]:
        line = lines[i]
        delims = [('r#\"', 3), ('\"', 1), ('r#\'', 3), ('\'', 1)]
        for d, length in delims:
            idx = line.find(d)
            if idx != -1:
                lines[i] = line[:idx+length] + 'agents_md, ' + line[idx+length:]
                found_format = True
                break
        if found_format:
            break

if not found_format:
    print('Could not find the format! call in the target function')
    exit(1)

# 4. Write the changes back
with open(file_path, 'w') as f:
    f.writelines(lines)

print('Successfully injected agents.md into src/agent.rs')
