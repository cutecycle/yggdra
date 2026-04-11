#!/bin/bash
# Quick setup for Ollama ↔ OpenRouter proxy

set -e

PLIST_PATH="$HOME/Library/LaunchAgents/com.yggdra.openrouter-proxy.plist"
SERVICE_NAME="com.yggdra.openrouter-proxy"

case "${1:-help}" in
  setup)
    echo "🔧 OpenRouter Proxy Setup"
    echo ""
    echo "Enter your OpenRouter API key (from https://openrouter.ai/keys):"
    read -s API_KEY
    
    if [[ ! "$API_KEY" =~ ^sk-or-v1- ]]; then
      echo "❌ Invalid API key format (should start with sk-or-v1-)"
      exit 1
    fi
    
    sed -i '' "s|sk-or-v1-YOUR_API_KEY_HERE|$API_KEY|" "$PLIST_PATH"
    echo "✅ API key saved"
    
    launchctl load "$PLIST_PATH"
    echo "✅ Service loaded"
    
    sleep 2
    if curl -s http://localhost:11435/health > /dev/null 2>&1; then
      echo "✅ Proxy is running on http://localhost:11435"
      echo ""
      echo "🎯 Use with yggdra:"
      echo "   export OLLAMA_ENDPOINT=http://localhost:11435"
      echo "   yggdra"
    else
      echo "⏳ Proxy starting... (check logs in 5 seconds)"
      echo "   tail /tmp/ollama-openrouter-proxy.log"
    fi
    ;;
    
  start)
    launchctl load "$PLIST_PATH"
    echo "✅ Service started"
    ;;
    
  stop)
    launchctl unload "$PLIST_PATH"
    echo "✅ Service stopped"
    ;;
    
  status)
    if launchctl list "$SERVICE_NAME" > /dev/null 2>&1; then
      echo "🟢 Service is running"
      curl -s http://localhost:11435/health | jq '.'
    else
      echo "🔴 Service is not running"
    fi
    ;;
    
  logs)
    echo "📋 Standard output:"
    tail -f /tmp/ollama-openrouter-proxy.log
    ;;
    
  *)
    echo "Usage: $0 {setup|start|stop|status|logs}"
    echo ""
    echo "Commands:"
    echo "  setup   - Configure API key and start service"
    echo "  start   - Start the proxy service"
    echo "  stop    - Stop the proxy service"
    echo "  status  - Check if service is running"
    echo "  logs    - Tail service logs"
    ;;
esac
