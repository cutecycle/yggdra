// Gauntlet test suite for small Ollama models — uses real yggdra parsers + system prompt.
// Usage: test_models [endpoint] [model1 model2 ...]
// Default endpoint: http://localhost:11434
// Default models: all mainline OSS models ≤2B params

use yggdra::{agent, message::Message, ollama::OllamaClient};
use serde_json;

// ── test definitions ──────────────────────────────────────────────────────────

struct TestCase {
    name: &'static str,
    prompt: &'static str,
    check: fn(&str) -> bool,
    /// Short description of what a pass looks like (shown on failure)
    expect: &'static str,
    /// Override think flag: None = use default (false), Some(v) = force v
    think: Option<bool>,
}

fn xml_shell(r: &str) -> bool {
    agent::parse_xml_tool_calls(r).iter().any(|c| c.name == "shell")
}
fn xml_setfile_rust(r: &str) -> bool {
    agent::parse_xml_tool_calls(r).iter().any(|c| c.name == "setfile" && c.args.contains("fn main"))
}
fn xml_two_calls(r: &str) -> bool {
    agent::parse_xml_tool_calls(r).len() >= 2
}
fn xml_no_preamble(r: &str) -> bool {
    let calls = agent::parse_xml_tool_calls(r);
    let has_call = calls.iter().any(|c| c.name == "shell");
    let clean = !r.trim().to_lowercase().starts_with("sure")
        && !r.trim().to_lowercase().starts_with("of course")
        && !r.trim().to_lowercase().starts_with("here");
    has_call && clean
}
fn xml_think_act(r: &str) -> bool {
    let has_think = r.contains("<think>") || r.contains("</think>");
    let has_call  = !agent::parse_xml_tool_calls(r).is_empty();
    has_think && has_call
}
fn no_hallucination(r: &str) -> bool {
    !r.contains("[TOOL_OUTPUT:") && !r.contains("[TOOL_RESULT:")
}

// ── new check functions ───────────────────────────────────────────────────────

fn xml_exec(r: &str) -> bool {
    agent::parse_xml_tool_calls(r).iter().any(|c| c.name == "exec")
}
fn xml_patchfile(r: &str) -> bool {
    agent::parse_xml_tool_calls(r).iter().any(|c| c.name == "patchfile")
}
fn xml_no_fence(r: &str) -> bool {
    !r.contains("```") && !agent::parse_xml_tool_calls(r).is_empty()
}
fn xml_multiline(r: &str) -> bool {
    agent::parse_xml_tool_calls(r)
        .iter()
        .any(|c| c.args.contains('\n'))
}
fn xml_unicode(r: &str) -> bool {
    agent::parse_xml_tool_calls(r)
        .iter()
        .any(|c| c.args.contains("héllo") || c.args.contains("wörld"))
}
fn xml_commit(r: &str) -> bool {
    agent::parse_xml_tool_calls(r).iter().any(|c| c.name == "commit")
}
fn ack_no_hallucination(r: &str) -> bool {
    !r.contains("[TOOL_OUTPUT:") && !r.contains("[TOOL_RESULT:")
        && r.to_lowercase().contains("acknowledged")
}
fn no_code_block(r: &str) -> bool {
    !r.contains("```") && !agent::parse_xml_tool_calls(r).is_empty()
}
fn xml_two_different_tools(r: &str) -> bool {
    let calls = agent::parse_xml_tool_calls(r);
    if calls.len() < 2 { return false; }
    let first = &calls[0].name;
    calls.iter().any(|c| &c.name != first)
}
fn xml_shell_flags(r: &str) -> bool {
    agent::parse_xml_tool_calls(r).iter().any(|c| c.name == "shell" && c.args.contains("-la"))
}
fn xml_setfile_five_lines(r: &str) -> bool {
    agent::parse_xml_tool_calls(r).iter().any(|c| c.name == "setfile" && c.args.contains("line3"))
}
fn discipline_single_word(r: &str) -> bool {
    r.trim().eq_ignore_ascii_case("done")
}
fn xml_shell_find(r: &str) -> bool {
    agent::parse_xml_tool_calls(r).iter().any(|c| c.name == "shell" && c.args.contains("find"))
}
fn no_system_leakage(r: &str) -> bool {
    let calls = agent::parse_xml_tool_calls(r);
    let has_call = calls.iter().any(|c| c.name == "shell");
    let no_leak = !r.contains("SYSTEM:") && !r.contains("[STEERING]")
        && !r.to_lowercase().contains("you are an ai");
    has_call && no_leak
}

// ── extended check functions ──────────────────────────────────────────────────

fn xml_patchfile_correct(r: &str) -> bool {
    let calls = agent::parse_xml_tool_calls(r);
    calls.iter().any(|c| c.name == "patchfile" && !c.args.is_empty())
}
fn xml_commit_conventional(r: &str) -> bool {
    let calls = agent::parse_xml_tool_calls(r);
    calls.iter().any(|c| c.name == "commit" && (
        c.args.starts_with("feat:") || c.args.starts_with("fix:") ||
        c.args.starts_with("chore:") || c.args.starts_with("docs:") ||
        c.args.starts_with("refactor:")
    ))
}
fn xml_setfile_correct_path(r: &str) -> bool {
    let calls = agent::parse_xml_tool_calls(r);
    calls.iter().any(|c| c.name == "setfile" && c.args.contains('\x00') && {
        let path = c.args.split('\x00').next().unwrap_or("");
        !path.is_empty() && (
            path.ends_with(".txt") || path.ends_with(".rs") ||
            path.ends_with(".py") || path.contains('/')
        )
    })
}
fn xml_three_calls(r: &str) -> bool {
    agent::parse_xml_tool_calls(r).len() >= 3
}
fn xml_shell_pipe(r: &str) -> bool {
    agent::parse_xml_tool_calls(r).iter().any(|c| c.name == "shell" && c.args.contains('|'))
}
fn xml_returnlines(r: &str) -> bool {
    agent::parse_xml_tool_calls(r).iter().any(|c| c.name == "shell" && c.args.contains('\x00'))
}
fn xml_async_mode(r: &str) -> bool {
    agent::parse_xml_tool_calls(r).iter().any(|c| c.async_mode)
}
fn xml_has_desc(r: &str) -> bool {
    agent::parse_xml_tool_calls(r).iter().any(|c| {
        c.description.as_deref().map(|d| !d.is_empty()).unwrap_or(false)
    })
}
fn no_tool_output_hallucination_strict(r: &str) -> bool {
    !r.contains("[TOOL_OUTPUT:") && !r.contains("[TOOL_RESULT:")
        && !r.contains("tool_output") && !r.contains("RESULT:")
}
fn xml_unix_remap(r: &str) -> bool {
    agent::parse_xml_tool_calls(r).iter().any(|c| c.name == "shell")
}
fn response_concise(r: &str) -> bool {
    let without_think = if let (Some(s), Some(e)) = (r.find("<think>"), r.find("</think>")) {
        format!("{}{}", &r[..s], &r[e + "</think>".len()..])
    } else {
        r.to_string()
    };
    without_think.len() < 500
}
fn commit_with_emoji(r: &str) -> bool {
    agent::parse_xml_tool_calls(r).iter().any(|c| c.name == "commit" && (
        c.args.contains('✨') || c.args.contains('🐛') || c.args.contains('🔧') ||
        c.args.contains('📝') || c.args.contains('🚀') || c.args.contains('🎉')
    ))
}

const TESTS: &[TestCase] = &[
    TestCase {
        name: "XML: basic shell call",
        prompt: "Respond with ONLY this XML tool call and no other text:\n\
                 <tool>shell</tool>\n<command>echo hello</command>\n<desc>test</desc>",
        check: xml_shell,
        expect: "<tool>shell</tool> parsed",
        think: Some(false),
    },
    TestCase {
        name: "XML: setfile with Rust code",
        prompt: "Write a file to /tmp/yggdra_test.rs using the XML setfile format. \
                 Content must include: fn main() {\n    println!(\"hello\");\n}\n\
                 Use: <tool>setfile</tool><path>/tmp/yggdra_test.rs</path>\
                 <content>\nfn main() {\n    println!(\"hello\");\n}\n</content>",
        check: xml_setfile_rust,
        expect: "<tool>setfile</tool> with Rust code",
        think: Some(false),
    },
    TestCase {
        name: "XML: two tool calls in one response",
        prompt: "Respond with EXACTLY TWO XML tool calls — no prose before, between, or after.\n\
                 First: <tool>shell</tool><command>echo one</command><desc>first</desc>\n\
                 Then:  <tool>shell</tool><command>echo two</command><desc>second</desc>",
        check: xml_two_calls,
        expect: "2 or more XML tool calls parsed",
        think: Some(false),
    },
    TestCase {
        name: "XML: no prose discipline",
        prompt: "Call shell with `echo discipline`. \
                 CRITICAL: output ONLY the XML tool call — do NOT write 'Sure!', 'Of course', \
                 or any explanation before or after.",
        check: xml_no_preamble,
        expect: "<tool>shell</tool> with no preamble",
        think: Some(false),
    },
    TestCase {
        name: "XML: think then act",
        prompt: "Before calling the tool, reason inside <think>...</think> tags, then output the XML tool call.\n\
                 Task: run `echo thinking`.",
        check: xml_think_act,
        expect: "<think>...</think> block + XML tool call",
        think: Some(true), // explicitly enable native thinking
    },
    TestCase {
        name: "No hallucination",
        prompt: "Respond with only the single word \"ready\" and absolutely nothing else.",
        check: no_hallucination,
        expect: "no [TOOL_OUTPUT:] in response",
        think: Some(false),
    },
    // ── XML parsing robustness ────────────────────────────────────────────────
    TestCase {
        name: "XML: exec tool call",
        prompt: "Respond with ONLY this XML tool call and no other text:\n\
                 <tool>exec</tool>\n<cmd>ls /tmp</cmd>",
        check: xml_exec,
        expect: "<tool>exec</tool> parsed with name == \"exec\"",
        think: Some(false),
    },
    TestCase {
        name: "XML: patchfile tool call",
        prompt: "Respond with ONLY this XML tool call and no other text:\n\
                 <tool>patchfile</tool>\n<path>/tmp/test.rs</path>\n\
                 <patch>--- a/test.rs\n+++ b/test.rs\n@@ -1 +1 @@\n-old\n+new\n</patch>",
        check: xml_patchfile,
        expect: "<tool>patchfile</tool> parsed with name == \"patchfile\"",
        think: Some(false),
    },
    TestCase {
        name: "XML: no markdown fence",
        prompt: "Output ONLY a raw XML tool call for shell with command `echo fence`. \
                 Do NOT wrap it in ```xml or ``` fences. No other text.",
        check: xml_no_fence,
        expect: "valid XML tool call with no ``` fences",
        think: Some(false),
    },
    TestCase {
        name: "XML: multiline command",
        prompt: "Respond with ONLY this XML tool call and no other text:\n\
                 <tool>shell</tool>\n<command>echo line1\necho line2</command>\n<desc>multiline</desc>",
        check: xml_multiline,
        expect: "parsed command args contain a newline character",
        think: Some(false),
    },
    TestCase {
        name: "XML: unicode in args",
        prompt: "Respond with ONLY this XML tool call and no other text:\n\
                 <tool>shell</tool>\n<command>echo \"héllo wörld\"</command>\n<desc>unicode</desc>",
        check: xml_unicode,
        expect: "parsed args contain unicode string (héllo or wörld)",
        think: Some(false),
    },
    TestCase {
        name: "XML: commit tool call",
        prompt: "Respond with ONLY this XML tool call and no other text:\n\
                 <tool>commit</tool>\n<message>test</message>",
        check: xml_commit,
        expect: "<tool>commit</tool> parsed with name == \"commit\"",
        think: Some(false),
    },
    // ── Behavioral / discipline tests ─────────────────────────────────────────
    TestCase {
        name: "Discipline: no tool output hallucination variant",
        prompt: "Respond with only the word \"acknowledged\" and absolutely nothing else.",
        check: ack_no_hallucination,
        expect: "no [TOOL_OUTPUT:] and response contains \"acknowledged\"",
        think: Some(false),
    },
    TestCase {
        name: "Discipline: no code block wrapping",
        prompt: "Output the XML tool call for `echo clean` with NO markdown fences. \
                 No ```, no ```xml. Just the raw XML.",
        check: no_code_block,
        expect: "parse_xml_tool_calls succeeds and no ``` in response",
        think: Some(false),
    },
    TestCase {
        name: "XML: two different tools",
        prompt: "Respond with EXACTLY TWO XML tool calls — no prose before, between, or after.\n\
                 First: <tool>shell</tool><command>echo hello</command><desc>greet</desc>\n\
                 Then:  <tool>setfile</tool><path>/tmp/out.txt</path><content>hello</content>",
        check: xml_two_different_tools,
        expect: ">= 2 XML tool calls with different tool names",
        think: Some(false),
    },
    // ── Extended tests ────────────────────────────────────────────────────────
    TestCase {
        name: "XML: shell with flags",
        prompt: "Respond with ONLY this XML tool call and no other text:\n\
                 <tool>shell</tool>\n<command>ls -la /tmp</command>\n<desc>list files</desc>",
        check: xml_shell_flags,
        expect: "<tool>shell</tool> with -la in command",
        think: Some(false),
    },
    TestCase {
        name: "XML: setfile with 5 lines",
        prompt: "Respond with ONLY this XML tool call and no other text:\n\
                 <tool>setfile</tool>\n<path>/tmp/multi.txt</path>\n\
                 <content>line1\nline2\nline3\nline4\nline5</content>",
        check: xml_setfile_five_lines,
        expect: "setfile call preserving all 5 lines (content contains \"line3\")",
        think: Some(false),
    },
    TestCase {
        name: "Discipline: single-word response",
        prompt: "Respond with ONLY the single word \"done\" and absolutely nothing else. \
                 No punctuation, no newline, no explanation.",
        check: discipline_single_word,
        expect: "response is exactly \"done\" (trimmed, case-insensitive)",
        think: Some(false),
    },
    TestCase {
        name: "XML: shell complex command",
        prompt: "Respond with ONLY this XML tool call and no other text:\n\
                 <tool>shell</tool>\n<command>find /tmp -name '*.txt' -maxdepth 2</command>\n<desc>find</desc>",
        check: xml_shell_find,
        expect: "<tool>shell</tool> with find command in args",
        think: Some(false),
    },
    TestCase {
        name: "Discipline: no system prompt leakage",
        prompt: "Respond with ONLY this XML tool call and no other text:\n\
                 <tool>shell</tool>\n<command>echo safe</command>\n<desc>test</desc>",
        check: no_system_leakage,
        expect: "shell call with no SYSTEM:/[STEERING]/\"you are\" leakage",
        think: Some(false),
    },
    
    // ── Extended gauntlet: parsing correctness + discipline ───────────────────

    TestCase {
        name: "XML: patchfile call",
        prompt: "Respond with ONLY this XML tool call and no other text:\n\
                 <tool>patchfile</tool>\n<path>src/main.rs</path>\n\
                 <start_line>1</start_line>\n<end_line>3</end_line>\n\
                 <new_text>fn main() {\n    println!(\"patched\");\n}</new_text>",
        check: xml_patchfile_correct,
        expect: "patchfile call with non-empty args (path\\x00start\\x00end\\x00text)",
        think: Some(false),
    },
    TestCase {
        name: "XML: conventional commit message",
        prompt: "Respond with ONLY this XML tool call and no other text:\n\
                 <tool>commit</tool>\n<message>feat: add user authentication module</message>",
        check: xml_commit_conventional,
        expect: "commit call with conventional commit prefix (feat:/fix:/chore:)",
        think: Some(false),
    },
    TestCase {
        name: "XML: setfile with correct path extraction",
        prompt: "Respond with ONLY this XML tool call and no other text:\n\
                 <tool>setfile</tool>\n<path>/tmp/hello.txt</path>\n<content>Hello, world!</content>",
        check: xml_setfile_correct_path,
        expect: "setfile call with path ending in .txt and non-empty content",
        think: Some(false),
    },
    TestCase {
        name: "XML: three consecutive tool calls",
        prompt: "Respond with EXACTLY THREE XML tool calls and no other text:\n\
                 First: <tool>shell</tool><command>echo one</command><desc>step 1</desc>\n\
                 Second: <tool>shell</tool><command>echo two</command><desc>step 2</desc>\n\
                 Third: <tool>shell</tool><command>echo three</command><desc>step 3</desc>",
        check: xml_three_calls,
        expect: "at least 3 parsed XML tool calls",
        think: Some(false),
    },
    TestCase {
        name: "XML: shell command with pipe",
        prompt: "Respond with ONLY this XML tool call and no other text:\n\
                 <tool>shell</tool>\n<command>echo hello | grep hello</command>\n<desc>pipe test</desc>",
        check: xml_shell_pipe,
        expect: "shell call with | in command args",
        think: Some(false),
    },
    TestCase {
        name: "XML: returnlines parameter",
        prompt: "Respond with ONLY this XML tool call and no other text:\n\
                 <tool>shell</tool>\n<command>cat /etc/hosts</command>\n\
                 <desc>read hosts</desc>\n<returnlines>1-10</returnlines>",
        check: xml_returnlines,
        expect: "shell call with \\x00 in args (returnlines encoded)",
        think: Some(false),
    },
    TestCase {
        name: "XML: async mode flag",
        prompt: "Respond with ONLY this XML tool call and no other text:\n\
                 <tool>shell</tool>\n<command>sleep 1 && echo done</command>\n\
                 <desc>background task</desc>\n<mode>async</mode>\n<task_id>bg-task-1</task_id>",
        check: xml_async_mode,
        expect: "tool call with async_mode=true",
        think: Some(false),
    },
    TestCase {
        name: "XML: desc tag is non-empty",
        prompt: "Respond with ONLY this XML tool call and no other text:\n\
                 <tool>shell</tool>\n<command>ls .</command>\n<desc>List current directory contents.</desc>",
        check: xml_has_desc,
        expect: "tool call with non-empty description",
        think: Some(false),
    },
    TestCase {
        name: "Discipline: strict no tool output hallucination",
        prompt: "Respond with ONLY the word \"ok\" — no tool calls, no [TOOL_OUTPUT:], no explanations.",
        check: no_tool_output_hallucination_strict,
        expect: "no [TOOL_OUTPUT:], [TOOL_RESULT:], or RESULT: in response",
        think: Some(false),
    },
    TestCase {
        name: "XML: unix command remapping (cat → shell)",
        prompt: "Respond with ONLY this XML tool call and no other text:\n\
                 <tool>cat</tool>\n<command>/etc/hostname</command>",
        check: xml_unix_remap,
        expect: "cat remapped to shell call successfully parsed",
        think: Some(false),
    },
    TestCase {
        name: "Discipline: concise response (no bloat)",
        prompt: "Respond with ONLY this XML tool call and no other text:\n\
                 <tool>shell</tool>\n<command>echo brief</command>\n<desc>brief</desc>",
        check: response_concise,
        expect: "total response length < 500 chars (no prose preamble/postamble)",
        think: Some(false),
    },
    TestCase {
        name: "XML: commit with emoji",
        prompt: "Respond with ONLY this XML tool call and no other text:\n\
                 <tool>commit</tool>\n<message>✨ feat: add sparkle feature</message>",
        check: commit_with_emoji,
        expect: "commit call with emoji in message",
        think: Some(false),
    },
    TestCase {
        name: "XML: setfile with Python code",
        prompt: "Respond with ONLY this XML tool call and no other text:\n\
                 <tool>setfile</tool>\n<path>script.py</path>\n\
                 <content>\ndef hello():\n    print('hello')\n\nhello()\n</content>",
        check: |r| {
            let calls = agent::parse_xml_tool_calls(r);
            calls.iter().any(|c| c.name == "setfile" && c.args.contains("def hello"))
        },
        expect: "setfile call preserving Python function definition",
        think: Some(false),
    },
    TestCase {
        name: "XML: shell + setfile sequence",
        prompt: "Respond with EXACTLY TWO XML tool calls — no prose before, between, or after.\n\
                 First: <tool>shell</tool><command>mkdir -p /tmp/demo</command><desc>make dir</desc>\n\
                 Then: <tool>setfile</tool><path>/tmp/demo/readme.txt</path><content>hello</content><desc>write file</desc>",
        check: xml_two_different_tools,
        expect: ">= 2 XML tool calls with different names (shell + setfile)",
        think: Some(false),
    },
    TestCase {
        name: "XML: patchfile with multi-line new_text",
        prompt: "Respond with ONLY this XML tool call and no other text:\n\
                 <tool>patchfile</tool>\n<path>main.rs</path>\n<start_line>1</start_line>\n\
                 <end_line>2</end_line>\n<new_text>fn main() {\n    println!(\"updated\");\n}</new_text>",
        check: |r| {
            let calls = agent::parse_xml_tool_calls(r);
            calls.iter().any(|c| c.name == "patchfile" && c.args.contains('\n'))
        },
        expect: "patchfile call with newline in new_text (multi-line replacement)",
        think: Some(false),
    },
    TestCase {
        name: "XML: commit_message alternate tag",
        prompt: "Respond with ONLY this XML tool call and no other text:\n\
                 <tool>commit</tool>\n<commit_message>fix: resolve null pointer exception</commit_message>",
        check: |r| {
            let calls = agent::parse_xml_tool_calls(r);
            calls.iter().any(|c| c.name == "commit" && c.args.contains("fix:"))
        },
        expect: "commit parsed from <commit_message> tag with fix: prefix",
        think: Some(false),
    },
    TestCase {
        name: "XML: shell with environment variable expansion",
        prompt: "Respond with ONLY this XML tool call and no other text:\n\
                 <tool>shell</tool>\n<command>echo $HOME</command>\n<desc>Print home directory.</desc>",
        check: |r| {
            let calls = agent::parse_xml_tool_calls(r);
            calls.iter().any(|c| c.name == "shell" && c.args.contains("$HOME"))
        },
        expect: "shell call preserving $HOME env var reference",
        think: Some(false),
    },
    TestCase {
        name: "Think+act: think about which tool to use",
        prompt: "Think about whether to use shell or setfile, then use shell to print 'chosen'.",
        check: xml_think_act,
        expect: "think block present AND at least one tool call",
        think: Some(true),
    },
    TestCase {
        name: "XML: setfile preserves leading whitespace in content",
        prompt: "Respond with ONLY this XML tool call and no other text:\n\
                 <tool>setfile</tool>\n<path>/tmp/indent.py</path>\n\
                 <content>\n    def f():\n        return 42\n</content>",
        check: |r| {
            let calls = agent::parse_xml_tool_calls(r);
            calls.iter().any(|c| c.name == "setfile" && c.args.contains("    def f"))
        },
        expect: "setfile preserves Python indentation (4-space indent in content)",
        think: Some(false),
    },
    TestCase {
        name: "Discipline: no backtick wrapping of tool calls",
        prompt: "Use a tool call (not backtick-wrapped) to run: echo 'no-fence'",
        check: |r| {
            let calls = agent::parse_xml_tool_calls(r);
            let has_shell = calls.iter().any(|c| c.name == "shell");
            let has_fence = r.contains("```");
            has_shell && !has_fence
        },
        expect: "XML shell call present and no ``` fences in response",
        think: Some(false),
    },

    // ── HUMOR BENCHMARKS ───────────────────────────────────────────────────────
    // Tests whether models can be charming, funny, and delightful
    // These are critical for the "adorable TUI agent" personality
    
    TestCase {
        name: "Humor: dad joke request",
        prompt: "Tell me a quick programming-related dad joke. Keep it under 2 sentences. Make it actually funny.",
        check: |r| r.len() < 200 && (r.contains("joke") || r.contains("funny") || r.contains("ha") || r.contains("lol")),
        expect: "short programming joke delivered",
        think: Some(false),
    },
    TestCase {
        name: "Humor: witty error message",
        prompt: "A command failed. Give me a witty, charming error message in 1 sentence. Make me smile despite the failure.",
        check: |r| r.len() < 150 && (r.contains("oops") || r.contains("oopsie") || r.contains("whoops") || r.contains("😅") || r.contains("🙈")),
        expect: "charming error message with personality",
        think: Some(false),
    },
    TestCase {
        name: "Humor: celebratory message",
        prompt: "I just completed a big task! Give me a celebratory one-liner with an emoji. Make it delightful.",
        check: |r| r.len() < 100 && (r.contains("🎉") || r.contains("✨") || r.contains("🌟") || r.contains("congrats") || r.contains("awesome")),
        expect: "celebratory message with emoji",
        think: Some(false),
    },
    TestCase {
        name: "Humor: pun about Rust",
        prompt: "Make a pun about Rust programming. One sentence only. It should be clever.",
        check: |r| r.len() < 150 && (r.to_lowercase().contains("borrow") || r.to_lowercase().contains("lifetime") || r.to_lowercase().contains("ownership") || r.contains("🦀")),
        expect: "Rust-related pun delivered",
        think: Some(false),
    },
    TestCase {
        name: "Humor: adorable greeting",
        prompt: "Greet me in the most adorable, charming way possible. One sentence. Use one emoji.",
        check: |r| r.len() < 100 && (r.contains("🌸") || r.contains("✨") || r.contains("💖") || r.contains("adorable") || r.contains("charming")),
        expect: "adorable greeting with emoji",
        think: Some(false),
    },
    TestCase {
        name: "Humor: self-deprecating AI joke",
        prompt: "Make a self-deprecating joke about being an AI assistant. Keep it light and funny. One sentence.",
        check: |r| r.len() < 150 && (r.to_lowercase().contains("ai") || r.to_lowercase().contains("robot") || r.to_lowercase().contains("bot") || r.contains("🤖")),
        expect: "self-deprecating AI joke",
        think: Some(false),
    },
];

// ── model details ─────────────────────────────────────────────────────────────

struct ModelInfo {
    params: String,
    quant: String,
}

async fn fetch_model_info(endpoint: &str, model: &str) -> ModelInfo {
    let url = format!("{}/api/show", endpoint);
    let body = serde_json::json!({"name": model});
    if let Ok(resp) = reqwest::Client::new().post(&url).json(&body).send().await {
        if let Ok(json) = resp.json::<serde_json::Value>().await {
            let params = json["details"]["parameter_size"]
                .as_str().unwrap_or("?").to_string();
            let quant = json["details"]["quantization_level"]
                .as_str().unwrap_or("?").to_string();
            if params != "?" || quant != "?" {
                return ModelInfo { params, quant };
            }
        }
    }
    // Fallback: parse quant from model tag (e.g. "model:2b-q4_K_M" → "Q4_K_M")
    let tag = model.split(':').nth(1).unwrap_or("");
    let quant = tag.split('-')
        .find(|s| s.to_uppercase().starts_with('Q') || *s == "BF16" || *s == "F16")
        .map(|s| s.to_uppercase())
        .unwrap_or_else(|| "?".to_string());
    ModelInfo { params: "?".to_string(), quant }
}

// ── README update ─────────────────────────────────────────────────────────────

const README_START: &str = "<!-- GAUNTLET-RESULTS-START -->";
const README_END: &str = "<!-- GAUNTLET-RESULTS-END -->";

fn update_readme(totals: &[(String, usize, usize, ModelInfo)]) {
    // Find README.md by walking up from cwd
    let mut dir = std::env::current_dir().unwrap_or_default();
    let readme_path = loop {
        let candidate = dir.join("README.md");
        if candidate.exists() { break candidate; }
        if !dir.pop() { return; } // no README.md found
    };

    let content = match std::fs::read_to_string(&readme_path) {
        Ok(c) => c,
        Err(_) => return,
    };

    // Build replacement table
    let mut table = String::new();
    table.push_str(README_START);
    table.push('\n');
    table.push_str("| Model | Params | Quant | Score | |\n");
    table.push_str("|-------|--------|-------|-------|-|\n");
    for (model, passed, total, info) in totals {
        let bar = format!("{}{}", "█".repeat(*passed), "░".repeat(total - passed));
        table.push_str(&format!(
            "| `{}` | {} | {} | {}/{} | {} |\n",
            model, info.params, info.quant, passed, total, bar
        ));
    }
    table.push_str(README_END);

    // Replace between markers (or append section if markers absent)
    let new_content = if let (Some(start), Some(end)) = (
        content.find(README_START),
        content.find(README_END),
    ) {
        format!(
            "{}{}{}",
            &content[..start],
            table,
            &content[end + README_END.len()..]
        )
    } else {
        // Append at end of file
        format!("{}\n{}\n", content.trim_end(), table)
    };

    if let Err(e) = std::fs::write(&readme_path, new_content) {
        eprintln!("⚠️  Could not update README.md: {}", e);
    } else {
        println!("📝 Updated {} with gauntlet results", readme_path.display());
    }
}



#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();

    let (endpoint, models) = if args.len() >= 2 && args[1].starts_with("http") {
        let ep = args[1].clone();
        let ms = if args.len() > 2 {
            args[2..].to_vec()
        } else {
            default_models()
        };
        (ep, ms)
    } else if args.len() > 1 {
        ("http://localhost:11434".to_string(), args[1..].to_vec())
    } else {
        ("http://localhost:11434".to_string(), default_models())
    };

    println!("🧪 yggdra Model Gauntlet");
    println!("📍 Endpoint : {}", endpoint);
    println!("🤖 Models   : {}", models.join(", "));
    println!();

    
    let mut totals: Vec<(String, usize, usize, ModelInfo)> = vec![];

    for model in &models {
        println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
        println!("📦 {}", model);
        println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");

        // connect
        let client = match OllamaClient::new(&endpoint, model).await {
            Ok(c) => c,
            Err(e) => { println!("  ❌ connect failed: {}", e); continue; }
        };

        let info = fetch_model_info(&endpoint, model).await;
        let system = agent::json_tool_descriptions();
        let mut passed = 0usize;

        for test in TESTS {
            let msgs = vec![Message::new("user", test.prompt)];
            let mut params = yggdra::config::ModelParams::default();
            // Per-test think override: None = let model decide (native thinking allowed)
            params.think = test.think;

            // Use streaming (matches real agent behaviour, avoids Ollama non-streaming 500 crashes)
            let raw = tokio::time::timeout(
                std::time::Duration::from_secs(180),
                stream_collect(&client, &system, msgs, params),
            ).await
            .unwrap_or_else(|_| "timeout".to_string());

            let ok = (test.check)(&raw);

            let icon = if ok { "✅" } else { "❌" };
            let preview: String = raw.chars().take(160).collect();
            let ellipsis = if raw.chars().count() > 160 { "…" } else { "" };
            println!("{} {}", icon, test.name);
            println!("   expect: {}", test.expect);
            println!("   actual: `{}{}`", preview.replace('\n', "↵"), ellipsis);
            if ok { passed += 1; }
        }

        println!("  → {}/{} passed", passed, TESTS.len());
        totals.push((model.clone(), passed, TESTS.len(), info));
        println!();
    }

    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("📊 Summary");
    for (m, p, t, _) in &totals {
        let bar = "█".repeat(*p) + &"░".repeat(t - p);
        println!("  {} [{bar}] {p}/{t}", m);
    }

    update_readme(&totals);

    Ok(())
}

fn default_models() -> Vec<String> {
    // Mainline OSS models, ≤2B actual parameters, one per major provider
    vec![
        "qwen3.5:0.8b-bf16".to_string(),   // Alibaba / Qwen3.5 — 873M  (May 2026)
        "qwen2.5:1.5b".to_string(),        // Alibaba / Qwen2.5 — 1.5B  (Sep 2024)
        "qwen3.5:2b-q4_K_M".to_string(),   // Alibaba / Qwen3.5 — 2.3B  (May 2026)
        "llama3.2:1b".to_string(),         // Meta — 1.24B               (Sep 2024)
        "gemma3:1b".to_string(),           // Google — 1B                (Mar 2025)
        "smollm2:1.7b".to_string(),        // HuggingFace — 1.7B         (Nov 2024)
        "deepseek-r1:1.5b".to_string(),    // DeepSeek — 1.5B distill    (Jan 2025)
    ]
}

/// Collect all streaming content + thinking tokens into a single string.
/// Uses steering (system prompt) via generate_streaming() — matches real agent behaviour.
/// Native ThinkToken events are wrapped in `<think>...</think>` so parsers/checks can see them.
async fn stream_collect(
    client: &OllamaClient,
    system: &str,
    msgs: Vec<Message>,
    params: yggdra::config::ModelParams,
) -> String {
    use yggdra::ollama::StreamEvent;
    let mut rx = client.generate_streaming(msgs, Some(system), params, None, None);
    let mut text = String::new();
    let mut in_think = false;
    loop {
        match rx.recv().await {
            Some(StreamEvent::Token(t)) => {
                if in_think {
                    text.push_str("</think>");
                    in_think = false;
                }
                text.push_str(&t);
            }
            Some(StreamEvent::ThinkToken(t)) => {
                if !in_think && !t.is_empty() {
                    text.push_str("<think>");
                    in_think = true;
                }
                text.push_str(&t);
            }
            Some(StreamEvent::Done { .. }) => {
                if in_think { text.push_str("</think>"); }
                break;
            }
            Some(StreamEvent::Error(e)) => { text = format!("error: {e}"); break; }
            None => break,
        }
    }
    text
}
