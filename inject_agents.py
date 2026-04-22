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

# 3. Find the function and inject the variable at the start
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

# 4. Inject 'agents_md, ' as the first argument in BOTH format! calls
# We search within the function scope
found_branch1 = False
found_branch2 = False

for i in range(target_line_idx, len(lines)):
    # Branch 1: ShellOnly
    if not found_branch1 and 'format!(' in lines[i] and 'CapabilityProfile::ShellOnly' in "".join(lines[target_line_idx:i]):
        line = lines[i]
        # Find opening delimiter for the format string
        # Delimiters: r#" , " , r#' , '
        delims = [('r#"', 3), ('"', 1), ("r#'", 3), ("'", 1)]
        for d, length in delims:
            idx = line.find(d)
            if idx != -1:
                lines[i] = line[:idx+length] + 'agents_md, ' + line[idx+length:]
                found_branch1 = True
                break
        if found_branch1: continue

    # Branch 2: Else/Default
    # We look for the format! call that follows the 'else {' block
    if not found_branch2 and 'format!(' in lines[i] and 'else {' in "".join(lines[target_line_idx:i]):
        line = lines[i]
        delims = [('r#"', 3), ('"', 1), ("r#'", 3), ("'", 1)]
        for d, length in delims:
            idx = line.find(d)
            if idx != -1:
                lines[i] = line[:idx+length] + 'agents_md, ' + line[idx+length:]
                found_branch2 = True
                break
        if found_branch2: break

if not (found_branch1 and found_branch2):
    print(f'Injection failed. Branch1: {found_branch1}, Branch2: {found_branch2}')
    exit(1)

# 5. Write the changes back
with open(file_path, 'w') as f:
    f.writelines(lines)

print('Successfully injected agents.md into both branches of system_prompt_with_steering in src/agent.rs')
