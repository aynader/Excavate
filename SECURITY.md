# Security and privacy

Excavate reads your files and keeps an index of what's in them, so a bug that exposes that is treated as a security
bug. Please report these privately, not in a public issue:

- something of yours leaving the PC, or being sent anywhere other than [docs/PRIVACY.md](docs/PRIVACY.md) says;
- a password, card number, ID or other secret stored in the index, or shown in a preview unblurred, when Excavate
  should have found it;
- a preview showing while you present when it shouldn't;
- a way around Windows Hello for **Reveal** or **Found and hidden**;
- Excavate Bedrock passing on another account's files, or anything beyond names, sizes, dates and attributes, or a way
  to make it do anything else with its rights;
- an update that would be installed without a valid signature, or anything else that lets someone run code through
  Excavate.

## How to report

Use GitHub's private vulnerability reporting: on this repository's **Security** tab, choose **Report a vulnerability**.
Only the maintainers see the report.

Say what you did, what happened, the version (Settings → About), and your Windows version. Don't attach real secrets or
personal files: describe them, or make up an example that shows the same thing.

You'll get an answer within a week. Once it's fixed and released, the report can be published, with credit if you'd
like it.

## Versions

Only the latest release gets fixes. With **Update automatically** on (Settings → About), a copy moves to it by itself.
