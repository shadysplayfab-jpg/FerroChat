# Ferrochat 0.3.2

## What's changed

- GET requests used by REST calls, message search, and GIF search now retry HTTP 429 rate-limit responses up to two times.
- Retries wait for a valid numeric `Retry-After` or `X-RateLimit-Reset-After` value, capped at 60 seconds per wait.
- Non-GET requests are sent once and are never automatically retried.
- When the delay is missing, invalid, or over the cap, Ferrochat returns the rate-limit response without retrying.

The Windows release is unsigned. See the accompanying malware note for the source review and scan limitations.