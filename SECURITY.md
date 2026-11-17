# Security Policy

## Supported versions

During pre-alpha only the latest commit on `main` is supported. There are no
releases yet, and no backports. Fixes land on `main` only.

| Version | Supported |
| --- | --- |
| Latest `main` | Yes |
| Anything else | No |

This policy will change once tagged releases exist.

## Reporting a vulnerability

Report vulnerabilities through GitHub private vulnerability reporting on this
repository (the **Security** tab, then **Report a vulnerability**). Do not open a
public issue, and do not include exploit details in a pull request.

Please include:

- The affected blink version or commit
- Windows version, GPU model, and NVIDIA driver version
- A description of the issue and its impact
- Steps to reproduce, and a proof of concept if you have one

You can expect an initial response within a few days. This is an unreleased
project run by volunteers, so please allow reasonable time before disclosing
publicly. We will credit reporters in the fix announcement unless you ask us not
to.

## Scope

blink runs with the privileges of the user who launched it. It captures the
desktop, loads `nvEncodeAPI64.dll` from the installed NVIDIA driver, and writes
MP4 files. Areas of particular interest:

- Parsing of any data that is not ours: decoder or muxer inputs, config files,
  and MP4 container handling
- Memory safety in `blink-capture`, `blink-audio`, and `blink-encode`, which are
  the only crates that contain `unsafe` code
- Path handling for the clip output directory

Out of scope: issues that require an already-compromised machine, and social
engineering.
