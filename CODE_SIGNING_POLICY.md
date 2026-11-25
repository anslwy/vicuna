# Code Signing Policy

Free code signing provided by [SignPath.io](https://signpath.io/), certificate by [SignPath Foundation](https://signpath.org/).

## Signing Scope

Windows release binaries (`vicuna.exe`, bundled `lb.exe`, and the NSIS installer `Vicuna_<version>_x64-setup.exe`) are built exclusively on GitHub-hosted runners from this public repository's `main` branch via [`.github/workflows/release.yml`](.github/workflows/release.yml) and submitted to SignPath for Authenticode signing before publication.

macOS release binaries (`.dmg` and CLI) are signed with an Apple Developer ID certificate and notarized by Apple in the same workflow.

## Team Roles

- **Committers & Reviewers:** [@anslwy](https://github.com/anslwy)
- **Approvers:** [@anslwy](https://github.com/anslwy)

All signed releases require source code hosted in this public repository (`anslwy/vicuna`) under the [Apache-2.0](LICENSE) license and are built in reproducible GitHub Actions workflows without proprietary components.

## Privacy Policy

This program will not transfer any information to other networked systems unless specifically requested by the user or the person installing or operating it.

Specifically:
- Vicuna is a local-first application that stores chats and settings locally in SQLite (`vicuna.db`).
- Network requests occur only when explicitly configured or triggered by the user:
  - Connecting to a local or user-configured Ollama server, OpenRouter API, or remote `vicuna serve` host.
  - Invoking user-installed coding agent CLIs (Claude Code, Codex, Antigravity, OpenCode, `fx`, `pi`, or user-approved ACP plugins) with the user's own credentials.
  - Connecting to user-configured MCP servers or messaging bridges (such as Telegram or Discord).
  - Checking GitHub Releases for app updates (`latest.json`) or fetching the Ollama model catalog when requested.
- Vicuna collects no telemetry, analytics, or crash reports.
