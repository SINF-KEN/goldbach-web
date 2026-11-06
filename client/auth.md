# goldbach.net01.io auth.md

You are an agent. This document tells you how to get access to the Goldbach Verification Network
(https://goldbach.net01.io), a live check of Goldbach's conjecture from 4×10^18 to 5×10^18, and how
to register and use a credential (a device id) if you want to contribute compute.

Reading needs no credential at all. OAuth 2.0 / OIDC is not supported: there is no
`/.well-known/oauth-authorization-server` and no `/.well-known/oauth-protected-resource`. This file
is the self-contained source of truth for agent authentication.

## Supported registration methods

- `none` — every read endpoint, the MCP server and the Markdown pages are public. No registration.
- `anonymous` — a device registers itself with one unauthenticated POST and immediately gets a
  credential (credential type: `device_id`, an opaque string). No account, no human in the loop.
- `user_delegated`, identity assertions (ID-JAG) and verified email are not supported: there are no
  user accounts.

What you can read without a credential:

- `GET /board`, `/api/stats`, `/api/live`, `/api/results`, `/api/health`, `/api/version`
  (described in [/openapi.json](https://goldbach.net01.io/openapi.json))
- MCP: `POST https://goldbach.net01.io/mcp` (Streamable HTTP; card at
  [/.well-known/mcp/server-card.json](https://goldbach.net01.io/.well-known/mcp/server-card.json))
- any page as Markdown: send `Accept: text/markdown`

## Step 1 — Register and provision a credential

Only needed to contribute compute. Registration endpoint: `POST https://goldbach.net01.io/api/register`

```http
POST /api/register HTTP/1.1
Host: goldbach.net01.io
Content-Type: application/json

{"name": "my-agent-device", "gpu": "adapter description"}
```

Response:

```json
{"client": "<device id>"}
```

The `client` value is your credential (`device_id`). Nothing else is created and no email is sent.
Limits per address: 5 registrations at once, then one a minute; every POST 2 a second.

## Step 2 — Use the credential

Send the device id in the JSON body as `"client"` (not in a header) of the work requests:

```http
POST /api/lease HTTP/1.1
Host: goldbach.net01.io
Content-Type: application/json

{"client": "<device id>"}
```

The same field goes in `POST /api/heartbeat`, `/api/result`, `/api/release` and `/api/report`. An
unknown or banned id gets `401`; then register again. Do not send an `Authorization` header: it is
ignored.

A device must run the project's own verifier (the browser engine at https://goldbach.net01.io/,
WebGPU + WebAssembly): a slice counts only when a device on another network reproduces it bit for
bit, so results from any other program are rejected. The simple way for an agent to contribute is to
open the dashboard in a WebGPU browser and press "start" (or call the page's WebMCP tool
`goldbach_start`).

## Step 3 — Rotation and revocation

Device ids do not expire and there is nothing to rotate. To stop, stop sending requests (or
`POST /api/release` your current lease so the slice goes back to the pool at once); an unused id is
simply never used again. There is no revocation endpoint. The server bans ids whose results
disagree with other devices.
