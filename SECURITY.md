# Security policy

hyprstate reads process command lines and writes them to disk, and it launches
programs on restore. Issues such as secrets leaking into snapshots, redaction
bypasses, or a snapshot or config file that can make hyprstate run something
unexpected are security issues.

## Reporting

Please report vulnerabilities privately through
[GitHub security advisories](https://github.com/flaviomilan/hyprstate/security/advisories/new),
not as public issues. You should get a response within a week.

## Supported versions

Only the latest commit on `main` is supported.
