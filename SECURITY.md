# Security policy

## Supported versions

| Phase | Versions that receive security fixes |
|---|---|
| Before 1.0 (now) | The latest release only |
| From 1.0 | The latest minor release; the previous minor release also receives fixes for critical issues |

Glyphvane is in early development and has no releases yet.

## Reporting a vulnerability

Please **do not** open a public issue, discussion or pull request for a
security problem.

Report it privately through GitHub private vulnerability reporting:
<https://github.com/sorrelfox/glyphvane/security/advisories/new>
(the **Report a vulnerability** button on the repository's *Security* tab).

Include, where you can:

- the affected version (`glyphvane --version`) and operating system;
- a description of the issue and its impact;
- a minimal file, vault or set of steps that reproduces it;
- any suggested fix or mitigation.

## What to expect

| Step | Target |
|---|---|
| Acknowledgement of your report | Within 7 days |
| Assessment and a fix or mitigation plan | Within 30 days |
| Disclosure | Through a GitHub Security Advisory once a fix is available, with credit to the reporter unless you prefer otherwise |

If a report turns out not to be a vulnerability, we will explain why and may
ask you to file it as a regular issue.

## Scope

Glyphvane opens documents and folders ("vaults") that may come from untrusted
sources. **Untrusted vault content is in scope**, including:

- Markdown that crashes, hangs or exhausts memory in the parser or renderer;
- raw HTML that bypasses the sanitizer, runs script, or loads content it should not;
- links, wikilinks and embeds that reach files outside the intended location, or
  open external programs or URLs without the expected confirmation;
- images, SVG, PDF and other media that exploit a decoder;
- remote content loaded when it should be blocked.

Out of scope: problems that require an attacker who already controls the
user's account or machine, and vulnerabilities in third-party dependencies
that do not affect Glyphvane (please report those upstream; we track
advisories through `cargo-deny`).
