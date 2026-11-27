# Code signing policy

blink has applied to the [SignPath Foundation](https://signpath.org) for free
code signing. Until the application is approved, release MSIs are unsigned and
Windows SmartScreen warns about an unknown publisher.

Free code signing provided by [SignPath.io](https://about.signpath.io),
certificate by [SignPath Foundation](https://signpath.org).

## What is signed

Each release's `blink-<version>-x86_64.msi` and the `blink.exe` inside it.
Both are built from this repository by `.github/workflows/package-windows.yml`
on GitHub-hosted runners, from the release tag, and signed by
`.github/workflows/release.yml` in the same workflow run. Nothing built
elsewhere, and no third-party binary, is signed. The SignPath artifact
configuration is in
[`packaging/windows/signpath-artifact-configuration.xml`](../packaging/windows/signpath-artifact-configuration.xml).

## Team roles

| Role | Members |
| --- | --- |
| Committers and reviewers | [Blair McAlpine](https://github.com/blairmcalpine) |
| Approvers | [Blair McAlpine](https://github.com/blairmcalpine) |

Changes reach `main` only through pull requests that pass CI. Every signing
request is approved by hand in SignPath. All team members use multi-factor
authentication on GitHub and SignPath.

## Privacy policy

This program will not transfer any information to other networked systems
unless specifically requested by the user or the person installing or
operating it.

blink makes no network connections. Clips, configuration, and logs stay on
the local machine.
