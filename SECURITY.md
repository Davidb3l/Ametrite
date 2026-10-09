# Security Policy

Ametrite is a local tool: one binary (`amt`) that reads and writes a SQLite database in your repository's `.ametrite/` directory, speaks MCP over stdio to the agent that launched it, and optionally serves a web board. It has no cloud service and no accounts, and it makes no network requests unless you ask it to (`amt upgrade`). Security reports are still welcome and taken seriously.

## Supported versions

During the pre-1.0 era, only the latest `0.x` release receives security fixes. After v1.0, the most recent stable major version is supported.

## Reporting a vulnerability

**Do not open a public issue.** Report privately through GitHub: open the repository's [Security tab](https://github.com/Davidb3l/Ametrite/security) and choose **Report a vulnerability** (GitHub's private vulnerability reporting). Include the `amt --version`, your OS, and steps to reproduce.

We commit to:

- Acknowledging your report within **48 hours**.
- A coordinated disclosure window of **90 days** from acknowledgment.
- Crediting reporters in the release notes (unless you prefer to remain anonymous).

There is no bug bounty.

## What a release provides

Releases are built and published by [cargo-dist](https://github.com/axodotdev/cargo-dist) from this repository's release workflow (`.github/workflows/release.yml`) when a version tag (such as `v0.2.0`) is pushed. Each [GitHub release](https://github.com/Davidb3l/Ametrite/releases) contains:

- prebuilt archives for macOS (Apple Silicon and Intel), Linux (x86_64), and Windows (x86_64), plus a source tarball;
- a `.sha256` file for each archive and a combined `sha256.sum`;
- `dist-manifest.json`, cargo-dist's machine-readable description of the release;
- the shell installer (`amt-installer.sh`), the PowerShell installer (`amt-installer.ps1`), and the Homebrew formula (`amt.rb`), which is also pushed to the `Davidb3l/homebrew-tap` tap.

What each install path checks:

- **Homebrew** pins each archive's SHA-256 in the formula, and `brew` refuses a download that doesn't match.
- **The shell installer** has each archive's SHA-256 embedded and verifies the download against it when a `sha256sum` command is available. If `sha256sum` isn't on your `PATH`, it prints that it is skipping the check and installs anyway.
- **The PowerShell installer** does not verify a checksum. Verify by hand (below) if that matters to you.
- **`cargo install --git … --locked`** builds from source against the committed `Cargo.lock`, and Cargo checks each downloaded crate against the checksum recorded there.
- **`amt upgrade`** re-runs whichever of the above installed `amt` (Homebrew, `cargo install`, or the installer), so it inherits that path's checks. A binary it can't attribute to Homebrew or Cargo, such as one you extracted by hand, is upgraded with the shell installer, or the PowerShell installer on Windows.

**Releases are not signed.** There are no GPG or Sigstore signatures and no build-provenance attestations. The checksums are published in the same release as the archives, so they catch a corrupted or truncated download, not a tampered release. To verify an archive by hand:

```sh
TAG=v0.2.0; T=amt-aarch64-apple-darwin.tar.xz
curl -fLO "https://github.com/Davidb3l/Ametrite/releases/download/$TAG/$T"
curl -fLO "https://github.com/Davidb3l/Ametrite/releases/download/$TAG/$T.sha256"
shasum -a 256 -c "$T.sha256"     # or: sha256sum -c "$T.sha256"
```

Look for `OK` on the archive's line. The `.sha256` files end with a blank line, so both tools may also print a warning about one improperly formatted line; that warning is harmless.

## Threat model

What Ametrite is designed around, and how far each defense goes today:

- **Agents racing for the same work**: claims are atomic (`BEGIN IMMEDIATE`) and leased, so two agents can never hold the same issue. This is a coordination guarantee between cooperating agents, not an access control: any process that can open the workspace database can do anything an agent can.
- **The local database**: `.ametrite/` holds plain SQLite. Anyone with read access to your checkout can read your issues, notes, and decisions. It git-ignores itself by default so it is not committed by accident.
- **Prompt injection via board content**: issue bodies, comments, and notes are written by agents and humans and handed back to agents (`context`, `brief`, MCP tools). Ametrite stores and returns that text verbatim; it does not execute it. Your agent must treat it as data, not instructions.
- **The web board: not hardened today.** The board started by `amt serve` (default port 1776) has no authentication, and anything that can send it a request can read and change every registered workspace. Two gaps make that wider than it sounds:
  - it listens on every network interface, not only localhost, so other machines on your network can reach it;
  - it doesn't check the `Origin` or `Host` of a request, so a web page open in your browser can send it changes (and, through DNS rebinding, read from it). A firewall doesn't stop this, because the requests come from your own browser.

  Until both are fixed, run the board only while you are using it (`amt serve --uninstall` removes the login service), and not on a network you don't trust. The agent-facing surfaces (CLI and MCP over stdio) open no network port.

Out of scope:

- Malicious code or a malicious agent already running as your user (it can edit the database directly).
- Compromised LLM providers or agent runtimes.
