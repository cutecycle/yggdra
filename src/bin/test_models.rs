// Gauntlet test suite for small Ollama models — uses real yggdra parsers + system prompt.
// Usage: test_models [endpoint] [model1 model2 ...]
// Default endpoint: http://localhost:11434
// Default models: qwen3.5:0.8b-bf16  qwen3.5:2b-q4_K_M  qwen3.5:4b-q4_K_M

use yggdra::{agent, message::Message, ollama::OllamaClient};

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
fn json_shell(r: &str) -> bool {
    agent::parse_json_tool_calls(r).iter().any(|c| c.name == "shell")
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
fn json_two_calls(r: &str) -> bool {
    agent::parse_json_tool_calls(r).len() >= 2
}
fn json_no_preamble(r: &str) -> bool {
    let calls = agent::parse_json_tool_calls(r);
    let has_call = !calls.is_empty();
    let clean = !r.trim().to_lowercase().starts_with("sure")
        && !r.trim().to_lowercase().starts_with("of course")
        && !r.trim().to_lowercase().starts_with("here")
        && !r.trim().to_lowercase().starts_with("i'll")
        && !r.trim().to_lowercase().starts_with("i will");
    has_call && clean
}
fn json_exec(r: &str) -> bool {
    agent::parse_json_tool_calls(r).iter().any(|c| c.name == "exec")
}
fn json_setfile(r: &str) -> bool {
    agent::parse_json_tool_calls(r).iter().any(|c| c.name == "setfile")
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
        think: None, // allow native thinking; ThinkTokens are wrapped in <think> by stream_collect
    },
    TestCase {
        name: "JSON: basic shell call",
        prompt: "Respond with ONLY this JSON and nothing else:\n\
                 {\"tool_calls\":[{\"name\":\"shell\",\"parameters\":{\"command\":\"echo hello\"}}]}",
        check: json_shell,
        expect: "JSON tool_calls with shell",
        think: Some(false),
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
    // ── JSON tests ────────────────────────────────────────────────────────────
    TestCase {
        name: "JSON: two tool calls",
        prompt: "Respond with ONLY this JSON and nothing else:\n\
                 {\"tool_calls\":[{\"name\":\"shell\",\"parameters\":{\"command\":\"echo one\"}},\
                 {\"name\":\"shell\",\"parameters\":{\"command\":\"echo two\"}}]}",
        check: json_two_calls,
        expect: "parse_json_tool_calls returns >= 2 calls",
        think: Some(false),
    },
    TestCase {
        name: "JSON: no preamble discipline",
        prompt: "Output ONLY a JSON tool call for `echo hello` — no explanation, \
                 no 'Sure', no 'Here is', just the raw JSON object with tool_calls.",
        check: json_no_preamble,
        expect: "JSON tool call present and no preamble words",
        think: Some(false),
    },
    TestCase {
        name: "JSON: exec tool call",
        prompt: "Respond with ONLY this JSON and nothing else:\n\
                 {\"tool_calls\":[{\"name\":\"exec\",\"parameters\":{\"cmd\":\"ls /tmp\"}}]}",
        check: json_exec,
        expect: "JSON tool call with name == \"exec\"",
        think: Some(false),
    },
    TestCase {
        name: "JSON: setfile tool call",
        prompt: "Respond with ONLY this JSON and nothing else:\n\
                 {\"tool_calls\":[{\"name\":\"setfile\",\"parameters\":\
                 {\"path\":\"/tmp/test.txt\",\"content\":\"hello\"}}]}",
        check: json_setfile,
        expect: "JSON tool call with name == \"setfile\"",
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
];

// ── main ──────────────────────────────────────────────────────────────────────

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

    
    let mut totals: Vec<(String, usize, usize)> = vec![];

    for model in &models {
        println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
        println!("📦 {}", model);
        println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");

        // connect
        let client = match OllamaClient::new(&endpoint, model).await {
            Ok(c) => c,
            Err(e) => { println!("  ❌ connect failed: {}", e); continue; }
        };

        let system = agent::json_tool_descriptions();
        let mut passed = 0usize;

        for test in TESTS {
            let msgs = vec![Message::new("user", test.prompt)];
            let mut params = yggdra::config::ModelParams::default();
            // Per-test think override: None = let model decide (native thinking allowed)
            params.think = test.think;

            // Use streaming (matches real agent behaviour, avoids Ollama non-streaming 500 crashes)
            let raw = tokio::time::timeout(
                std::time::Duration::from_secs(45),
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
        totals.push((model.clone(), passed, TESTS.len()));
        println!();
    }

    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("📊 Summary");
    for (m, p, t) in &totals {
        let bar = "█".repeat(*p) + &"░".repeat(t - p);
        println!("  {} [{bar}] {p}/{t}", m);
    }

    Ok(())
}

fn default_models() -> Vec<String> {
    vec![
        "qwen3.5:0.8b-bf16".to_string(),
        "qwen3.5:2b-q4_K_M".to_string(),
        "qwen3.5:4b-q4_K_M".to_string(),
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
