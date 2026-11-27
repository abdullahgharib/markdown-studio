# Security Policy

## Supported Versions

Only the latest release line receives active security updates and patches:

| Version | Supported |
| :--- | :--- |
| `0.8.x` | :white_check_mark: Supported |
| `< 0.8.0` | :x: Unsupported |

Please upgrade to the latest release of **Markdown Studio** before reporting an issue if possible.

---

## Reporting a Vulnerability

**Please do not report security vulnerabilities through public GitHub Issues or pull requests.**

Instead, please report suspected vulnerabilities privately using GitHub's **Private Vulnerability Reporting**:

1. Navigate to the repository's **Security** tab -> **Advisories** -> **Report a vulnerability**, or open directly:
   <https://github.com/abdullahgharib/markdown-studio/security/advisories/new>
2. Provide as much relevant context as possible so we can reproduce and assess the report quickly:
   - **Markdown Studio version** and **operating system / architecture** (Windows, Linux, or macOS).
   - **Clear description** of the vulnerability and potential security impact.
   - **Minimal reproduction steps** (such as a small `.md`, `.html`, image/SVG, or IPC payload).
   - **Suggested remediation or patch** (optional).

### Response & Disclosure Timeline

- **Acknowledgment:** We aim to acknowledge new private vulnerability reports within **3–5 business days**.
- **Assessment & Fix:** We will investigate the report, determine affected versions, and work with you on a fix in a private advisory fork.
- **Coordinated Disclosure:** Once a patched release is available, we will publish the security advisory and release notes crediting you (unless you prefer to remain anonymous).

---

## Security Boundaries & In-Scope Areas

Reports involving bypasses of **Markdown Studio's** built-in default security boundaries are in scope, including:

- **Workspace & Path Confinement:** Directory traversal (`..`) escapes, symlink/junction escapes, or unauthorized access to host files/assets during document export (HTML, PDF, `.docx`), clipboard/image loading, or `.handoff` tab transfers.
- **Network & Remote Resource Protections:** Server-Side Request Forgery (SSRF) bypasses in remote image fetching across redirects, or loading private/loopback/link-local/metadata addresses.
- **Untrusted Document / Media Parsing & Export Sanitization:** Unbounded memory/CPU exhaustion or crashes (e.g., crafted `.svgz`/GIF/PNG images or deeply nested AST/Mermaid/Math inputs), or XSS/script-injection escapes in exported HTML/PDF/DOCX output or Rich-Text (`CF_HTML`) clipboard conversion.
- **Windows Shell Integration & IPC:** Privilege-boundary escapes (modifications outside `HKCU\Software\Classes`), untrusted executable resolution, or local single-instance IPC spoofing/tampering.
- **Default High-Risk Link & URI Blocks:** Bypasses that execute local executables/scripts, UNC network shares (`\\server\share`), NTFS Alternate Data Streams (`file.txt:stream`), or `javascript:` / `vbscript:` / `data:` URIs when those high-risk opt-in settings remain disabled (their safe default).

## Out of Scope

- Security consequences resulting directly from a user **explicitly enabling** high-risk link/URI opt-in toggles under **Settings -> Security** (such as allowing executables/scripts, UNC network shares, NTFS Alternate Data Streams, `javascript:`, `vbscript:`, or `data:` URIs) and clicking links in untrusted documents.
- Theoretical advisories in transitive dependencies whose vulnerable code paths are unreachable (for example, archive/PDF readers/extractors inside write-only exporters).
- Denial-of-service reports requiring unrealistic local machine privileges or user-controlled local settings file tampering.
