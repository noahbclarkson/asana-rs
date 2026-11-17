# Security policy

## Reporting a vulnerability

Please report a vulnerability privately, through GitHub's
[private vulnerability reporting](https://github.com/noahbclarkson/asana-rs/security/advisories/new)
for this repository, and not in a public issue, pull request or discussion. Include the
affected crate and version, what an attacker can do, and the smallest example that shows it.

You will get an answer within a week. Once a fix is released, the advisory is published and, if
you wish, credits you. A fixed release is also submitted to the
[RustSec advisory database](https://rustsec.org/).

Examples of what counts: a webhook delivery accepted without a valid signature, a secret
(access token, refresh token, client secret, webhook secret) reaching a log, an error message or
`Debug` output, or a request sent somewhere other than where it was configured.

## Never post a token

Asana access tokens, OAuth refresh tokens, client secrets and webhook secrets give access to
someone's Asana data. Never put one in an issue, a pull request, a log excerpt, a test or a
screenshot, even an expired one. Replace it with `<redacted>`. If you have posted one, revoke it
in Asana at once: a personal access token in the developer console, an OAuth grant through the
app's revoke endpoint, and a webhook by deleting it.

## Supported versions

Fixes go into the latest release. While the crates are `0.x`, that is the latest minor version
only.
