.PHONY: test deploy

test:
	cargo test

deploy:
	cargo install --path .
	cp com.akitenkrad.obsidian.ai-subscription-usage.plist ~/Library/LaunchAgents/
	launchctl bootout gui/$$(id -u)/com.akitenkrad.obsidian.ai-subscription-usage 2>/dev/null || true
	launchctl bootstrap gui/$$(id -u) ~/Library/LaunchAgents/com.akitenkrad.obsidian.ai-subscription-usage.plist
	cp com.akitenkrad.obsidian.ai-subscription-usage-codex.plist ~/Library/LaunchAgents/
	launchctl bootout gui/$$(id -u)/com.akitenkrad.obsidian.ai-subscription-usage-codex 2>/dev/null || true
	launchctl bootstrap gui/$$(id -u) ~/Library/LaunchAgents/com.akitenkrad.obsidian.ai-subscription-usage-codex.plist
	cp com.akitenkrad.obsidian.ai-subscription-usage-limits.plist ~/Library/LaunchAgents/
	launchctl bootout gui/$$(id -u)/com.akitenkrad.obsidian.ai-subscription-usage-limits 2>/dev/null || true
	launchctl bootstrap gui/$$(id -u) ~/Library/LaunchAgents/com.akitenkrad.obsidian.ai-subscription-usage-limits.plist
	cp com.akitenkrad.obsidian.ai-subscription-usage-codex-limits.plist ~/Library/LaunchAgents/
	launchctl bootout gui/$$(id -u)/com.akitenkrad.obsidian.ai-subscription-usage-codex-limits 2>/dev/null || true
	launchctl bootstrap gui/$$(id -u) ~/Library/LaunchAgents/com.akitenkrad.obsidian.ai-subscription-usage-codex-limits.plist
	@echo "AI subscription usage jobs deployed"
