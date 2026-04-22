import os

# 1. Load the content from our temp file
temp_file = '.agents_md_temp'
try:
    with open(temp_file, 'r') as f:
        content = f.read()
    # We use a very strong delimiter: r##" ... "##
    # We must ensure the content itself doesn't contain the sequence ##"
    # If it does, we escape it.
    safe_content = content.replace('##"', '###"')
    injection_var = f"    let agents_md = r##\"{safe_content}\"##;\n"
except Exception as e:
    print(f'Error reading {temp_file}: {e}')
    exit(1)

# 2. Read src/agent.rs
file_path = 'src/agent.rs'
with open(file_path, 'r') as f:
    lines = f.readlines()

# 3. Find the function
target_line_idx = -1
for i, line in enumerate(lines):
    if 'fn system_prompt_with_steering' in line:
        target_line_idx = i
        break

if target_line_idx == -1:
    print('Could not find system_prompt_with_steering function')
    exit(1)

# 4. Re-construct the file to be clean
# First, remove the failed previous attempt if it exists in the lines
# We look for the line that starts with "let agents_md = r#" and remove it
new_lines = []
skip_next = False
for i, line in enumerate(lines):
    if 'let agents_md = r#' in line and i > target_line_idx:
        continue
    new_lines.append(line)

# Now, insert the CORRECTED variable definition
# We insert it right after the function signature
new_lines.insert(target_line_idx + 1, injection_var)

# 5. Find the format! calls and inject 'agents_md, ' as the first argument
# We'll look specifically in the lines following the function start
found_count = 0
for i in range(target_line_idx + 1, len(new_lines)):
    if 'format!(' in new_lines[i]:
        line = new_lines[i]
        # We look for the opening of the format string: r##" or "
        # Since we are injecting into a format! call, it's likely a normal string " or r##"
        delims = [('r##"', 4), ('"', 1)]
        found_in_line = False
        for d, length in delims:
            idx = line.find(d)
            if idx != -1:
                new_lines[i] = line[:idx+length] + 'agents_md, ' + line[idx+length:]
                found_count += 1
                found_in_line = True
                break
        if found_in_line:
            continue

# 6. Write the changes back
with open(file_path, 'w') as f:
    f.writelines(new_lines)

print(f'Successfully fixed and injected agents.md into {found_count} format! calls in src/agent.rs')
