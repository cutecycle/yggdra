import os

# 1. Load the content from our temp file
temp_file = '.agents_md_temp'
try:
    with open(temp_file, 'r') as f:
        content = f.read()
except Exception as e:
    print(f'Error reading {temp_file}: {e}')
    exit(1)

# 2. Read src/agent.rs
file_path = 'src/agent.rs'
with open(file_path, 'r') as f:
    lines = f.readlines()

# 3. Find the function start
target_line_idx = -1
for i, line in enumerate(lines):
    if 'fn system_prompt_with_steering' in line:
        target_line_idx = i
        break

if target_line_idx == -1:
    print('Could not find system_prompt_with_steering function')
    exit(1)

# Insert the variable definition at the beginning of the function
lines.insert(target_line_idx + 1, f'    let agents_md = r#\'{content}\'#;\n')

# 4. Find ALL format! calls within this function and inject 'agents_md, '
# We'll track the scope by looking for the closing brace of the function.
# For simplicity, we'll just scan the next 200 lines.
found_count = 0
for i in range(target_line_idx, min(target_line_idx + 200, len(lines))):
    if 'format!(' in lines[i]:
        line = lines[i]
        # Look for opening delimiter for the format string
        # Delimiters: r#" , " , r#' , '
        delims = [('r#"', 3), ('"', 1), ("r#'", 3), ("'", 1)]
        for d, length in delims:
            idx = line.find(d)
            if idx != -1:
                lines[i] = line[:idx+length] + 'agents_md, ' + line[idx+length:]
                found_count += 1
                break

if found_count == 0:
    print('Could not find any format! calls in the function')
    exit(1)

# 5. Write the changes back
with open(file_path, 'w') as f:
    f.writelines(lines)

print(f'Successfully injected agents.md into {found_count} format! calls in src/agent.rs')
