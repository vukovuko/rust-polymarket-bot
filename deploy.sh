#!/bin/bash
set -euo pipefail

# ============================================================
# Polymarket Bot — VPS Deployment Script
# Run this on a fresh VPS (Debian/Ubuntu)
# ============================================================

echo "=== Polymarket Bot VPS Setup ==="

# 1. System updates
echo "[1/7] Updating system..."
sudo apt-get update && sudo apt-get upgrade -y

# 2. Install Docker
echo "[2/7] Installing Docker..."
if ! command -v docker &> /dev/null; then
    curl -fsSL https://get.docker.com | sh
    sudo usermod -aG docker "$USER"
    echo "Docker installed. You may need to log out and back in for group to take effect."
fi

# 3. Install Docker Compose plugin
echo "[3/7] Checking Docker Compose..."
if ! docker compose version &> /dev/null; then
    sudo apt-get install -y docker-compose-plugin
fi

# 4. Create project directory
echo "[4/7] Setting up project..."
mkdir -p ~/polymarket-bot/src/polymarket ~/polymarket-bot/src/strategies ~/polymarket-bot/logs
cd ~/polymarket-bot

# 5. Create .env from template if it doesn't exist
echo "[5/7] Checking .env..."
if [ ! -f .env ]; then
    cat > .env << 'ENVEOF'
# REQUIRED — fill these in
PRIVATE_KEY=0xYOUR_KEY_HERE

# Risk limits
MAX_TRADE_USD=5
MAX_DAILY_EXPOSURE=20
KILL_SWITCH_LOSS=10

# Strategy
ARB_THRESHOLD=0.02
MOMENTUM_THRESHOLD=0.0015
SPREAD_OFFSET=0.03

# API
POLY_API_URL=https://clob.polymarket.com
POLY_WS_URL=wss://ws-subscriptions-clob.polymarket.com/ws/market

# Telegram alerts (get from @BotFather)
TG_BOT_TOKEN=
TG_CHAT_ID=

# Logging
RUST_LOG=info
ENVEOF

    # Lock down .env permissions
    chmod 600 .env
    echo "Created .env — EDIT IT with your private key and Telegram tokens"
    echo "  nano ~/polymarket-bot/.env"
else
    echo ".env already exists, skipping"
fi

# 6. Secure the .env file
echo "[6/7] Securing secrets..."
chmod 600 .env

# Create .gitignore
cat > .gitignore << 'EOF'
.env
logs/
target/
EOF

# Create .dockerignore
cat > .dockerignore << 'EOF'
.env
logs/
target/
.git/
EOF

# 7. Summary
echo "[7/7] Setup complete!"
echo ""
echo "=== Next Steps ==="
echo "1. Edit .env with your secrets:"
echo "   nano ~/polymarket-bot/.env"
echo ""
echo "2. Add your PRIVATE_KEY (Polymarket wallet)"
echo "   Add TG_BOT_TOKEN and TG_CHAT_ID for alerts"
echo ""
echo "3. Copy project files (Cargo.toml, src/, Dockerfile, docker-compose.yml)"
echo "   Or use Claude Code to build them from CLAUDE.md"
echo ""
echo "4. Build and run:"
echo "   cd ~/polymarket-bot"
echo "   docker compose up -d --build"
echo "   docker compose logs -f"
echo ""
echo "5. Stop:"
echo "   docker compose down"
echo ""
echo "=== Security Reminders ==="
echo "- .env is chmod 600 (owner read/write only)"
echo "- .env is in .gitignore and .dockerignore"
echo "- Bot runs as non-root user inside container"
echo "- No ports are exposed — outbound connections only"
echo "- NEVER commit .env or your private key to git"
