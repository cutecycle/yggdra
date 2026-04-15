# OpenRouter Cloud Model Integration

Yggdra is configured to use Google's Gemma 4 model via OpenRouter API through a local Ollama-compatible proxy service.

## Architecture

```
yggdra
  ↓
localhost:11435 (ollama-api-proxy) ← Ollama API format
  ↓
api.openrouter.ai ← OpenRouter API format
  ↓
google/gemma-4-26b-a4b-it
```

## Service Status

The proxy service runs as a persistent macOS launchd daemon:

```bash
# Check if service is running
launchctl list | grep yggdra

# View recent logs
tail -50 /tmp/ollama-proxy.log

# Manually restart
launchctl unload ~/Library/LaunchAgents/com.yggdra.openrouter-proxy.plist
launchctl load ~/Library/LaunchAgents/com.yggdra.openrouter-proxy.plist
```

## Available Models

Models are defined in `models.json`:

- **gemma-4-26b** (primary, reliable) → `google/gemma-4-26b-a4b-it`
- **deepseek-r1** (free-tier, may be unavailable) → `deepseek/deepseek-r1-0528:free`
- **kimi-k2.5** (free-tier, may be unavailable) → `moonshotai/kimi-k2.5:free`

> **Note:** Free-tier OpenRouter models can go offline without notice.
> Only `gemma-4-26b` is consistently available.

To verify available models:

```bash
curl -s http://localhost:11435/api/tags | jq '.models[] | .name'
```

## Testing Inference

Test the full proxy → OpenRouter chain:

```bash
curl -s -X POST http://localhost:11435/api/chat \
  -H "Content-Type: application/json" \
  -d '{
    "model": "gemma-4-26b",
    "messages": [{"role": "user", "content": "What is 2+2?"}],
    "stream": false
  }' | jq '.message.content'
```

Expected: `"2 + 2 = 4"` (or similar)

## Using with Yggdra

Just run yggdra normally:

```bash
yggdra
```

It will automatically:
1. Connect to `http://localhost:11435` (proxy endpoint)
2. Use the `gemma-4-26b` model
3. Send all messages through OpenRouter API

## Configuration

### Yggdra Config
Located at `~/.yggdra/config.json`:

```json
{
  "ollama_endpoint": "http://localhost:11435",
  "model": "gemma-4-26b",
  "context_limit": 8000,
  "battery_low_percent": 30,
  "compression_threshold": 70
}
```

### OpenRouter API Key
Embedded in the wrapper script:
`/Users/cutecycle/Library/LaunchAgents/start-openrouter-proxy.sh`

The key is set as `OPENROUTER_API_KEY` environment variable.

### Service Configuration
Location: `/Users/cutecycle/Library/LaunchAgents/com.yggdra.openrouter-proxy.plist`

- Port: 11435
- Auto-restart: Yes (KeepAlive + RunAtLoad)
- Working directory: `/Users/cutecycle/source/repos/yggdra`

## Troubleshooting

### Service won't start
1. Check logs: `tail -100 /tmp/ollama-proxy.log`
2. Verify API key in wrapper script
3. Restart: `launchctl unload ... && launchctl load ...`

### No models available
1. Ensure `models.json` exists in yggdra directory
2. Restart the proxy service
3. Test: `curl -s http://localhost:11435/api/tags`

### Slow responses
- OpenRouter adds minimal latency
- Most delays are network-related or model-specific
- Deepseek-r1 can be slower due to reasoning

### API quota exceeded
- Monitor usage on https://openrouter.ai/account/usage
- Consider using free-tier models (deepseek-r1:free)
- Switch to local Ollama if needed (change config.json endpoint to `http://localhost:11434`)

## Switching Models

To use a different model:

```bash
# Option 1: Change config
# Edit ~/.yggdra/config.json
"model": "deepseek-r1"  # or "kimi-k2"

# Option 2: Use /model command in yggdra
/model deepseek-r1
```

## Adding More Models

Add entries to `models.json`:

```json
{
  "gpt-4o": {
    "provider": "openai",
    "model": "openai/gpt-4o"
  }
}
```

Then restart the service:

```bash
launchctl unload ~/Library/LaunchAgents/com.yggdra.openrouter-proxy.plist
launchctl load ~/Library/LaunchAgents/com.yggdra.openrouter-proxy.plist
```

## Fallback to Local Ollama

If OpenRouter is unavailable, revert to local Ollama:

```bash
# Edit ~/.yggdra/config.json
{
  "ollama_endpoint": "http://localhost:11434",
  "model": "qwen2:0.5b"
}
```

Requires: Ollama running on port 11434 with downloaded models

## Cost Tracking

Monitor costs on OpenRouter:
- Dashboard: https://openrouter.ai/account/usage
- Most models priced per 1M tokens
- Free-tier models available

## Files

- `models.json` - Model mappings for proxy
- `~/.yggdra/config.json` - Yggdra configuration
- `~/Library/LaunchAgents/com.yggdra.openrouter-proxy.plist` - Service config
- `~/Library/LaunchAgents/start-openrouter-proxy.sh` - Service startup script
