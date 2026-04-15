# Model Inference Validation Results

## Date
2026-04-14

## Summary
Successfully downloaded and validated qwen3.5:4b and qwen3.5:2b as reliable standard models for yggdra agentic workflows.

## Test Results

### Non-Heretic Models (All Passing ✅)
| Model | Plain | Artifacts | Tool-Call | Hallucination | Status |
|-------|-------|-----------|-----------|---------------|--------|
| qwen3.5:4b | ✅ | ✅ | ✅ | ✅ | **Production-ready** |
| qwen3.5:2b | ✅ | ✅ | ✅ | ✅ | **Production-ready** |
| qwen3.5:9b | ✅ | ✅ | ✅ | ✅ | **Stable** |
| gemma-4-26b | ✅ | ✅ | ✅ | ✅ | **OpenRouter** |

**Total: 16/16 tests passing**

### Heretic Models (Problematic ❌)
| Model | Plain | Artifacts | Tool-Call | Hallucination | Status |
|-------|-------|-----------|-----------|---------------|--------|
| qwen3.5-heretic-2b | ✅ | ✅ | ❌ | ✅ | Tool format issues |
| qwen3.5-heretic-4b | ✅ | ❌ | ❌ | ✅ | Training artifacts + tool issues |
| qwen3.5-heretic-9b | ✅ | ❌ | ❌ | ✅ | Training artifacts + tool issues |
| qwen3.5-heretic-27b | ⏭️ | ⏭️ | ⏭️ | ⏭️ | Unavailable (OOM?) |

**Total: 5 failures across heretic suite**

### Unavailable Models ⏭️
- deepseek-r1 (OpenRouter free tier - endpoint removed)
- kimi-k2.5 (OpenRouter free tier - endpoint removed)

## Key Findings

1. **Non-heretic qwen3.5 models are reliable**: Both 4b and 2b variants pass all tests, confirming they follow instructions correctly without hallucinating.

2. **Heretic models fundamentally unreliable for tool use**: They don't respect tool format instructions and emit training artifacts like `<|endoftext|>` mid-response, making them unsuitable for agentic workflows.

3. **JSON tool format recommendation**: With JSON-based tool calling implemented in agent.rs, the standard models should work even better than text-based `[TOOL:]` format.

4. **Model selection for yggdra**:
   - **Primary**: qwen3.5:9b (best balance of quality/speed for local)
   - **Fallback**: qwen3.5:4b or qwen3.5:2b (for resource-constrained environments)
   - **OpenRouter**: gemma-4-26b (high-quality external)
   - **Avoid**: All heretic models for production agentic workflows

## Recommendations

1. Update steering prompts to prefer JSON format for standard models
2. Add model blacklist for heretic variants in production builds
3. Document model selection strategy in user guide
4. Monitor gemma-4-26b OpenRouter free tier availability

## Test Infrastructure

- Inference test script: `scripts/test-inference.sh` (comprehensive, covers all 4 criteria)
- Unit tests: `tests/model_compat.rs` (141 tests, all passing)
- JSON tool parsing: `src/agent.rs` with robust schema validation
