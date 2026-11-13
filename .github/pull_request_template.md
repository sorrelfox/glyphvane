## Summary

<!-- What does this change do, and why? Link related issues with "Fixes #123" or "Refs #123". -->

## Checklist

- [ ] Every commit is signed off (`git commit -s`), certifying the [Developer Certificate of Origin](https://developercertificate.org/).
- [ ] Commit messages follow [Conventional Commits](https://www.conventionalcommits.org/) with a scope, e.g. `fix(links): …`.
- [ ] `cargo xtask ci` passes locally (fmt, SPDX headers, dependency rules, clippy, tests).
- [ ] Tests added or updated for the change (or explained below why none are needed).
- [ ] User-facing docs updated where behaviour changes (`docs/user/`, `README.md`, `CONTRIBUTING.md`).
- [ ] New source files start with the SPDX header.

## Performance impact

<!-- Does this touch a hot path (parsing, layout, rendering, indexing, scrolling)?
     Describe the expected impact and include benchmark numbers if you have them.
     Write "None expected" if it does not apply. -->

## Screenshots (UI changes only)

<!-- UI changes must be shown in all three themes. Colours come from theme tokens, never hardcoded. -->

| Light | Semi-light | Dark |
|---|---|---|
|  |  |  |
