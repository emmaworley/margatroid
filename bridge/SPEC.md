# Claude Code Remote Control Bridge Protocol Specification

Reverse-engineered from the Claude Code runner binary (v2.1.79) and verified
end-to-end with a working Rust echo server. March 2026.

---

## Overview

The bridge protocol lets an external program ("worker") replace the Claude Code
agentic session. The worker registers as a "bridge environment", receives user
messages from claude.ai/code, and sends responses back. All communication goes
through Anthropic's API servers — there is no direct connection between the
browser and the worker.

```
┌─────────────┐        HTTPS         ┌───────────────────┐        SSE/POST       ┌──────────┐
│ claude.ai   │ ◄──────────────────► │  Anthropic API    │ ◄───────────────────► │  Worker  │
│   /code     │   WebSocket subscribe│  api.anthropic.com│   SSE stream + REST   │ (bridge) │
└─────────────┘                      └───────────────────┘                       └──────────┘
```

## Credential Files

```
~/.claude/.credentials.json
{
  "claudeAiOauth": {
    "accessToken": "eyJhb..."       ← OAuth access token
  }
}

~/.claude.json
{
  "oauthAccount": {
    "organizationUuid": "abc-123"   ← organization UUID
  }
}
```

## Authentication

Three different tokens are used depending on the endpoint:

| Token                    | Source                           | Used for                              |
|--------------------------|----------------------------------|---------------------------------------|
| **OAuth access token**   | `~/.claude/.credentials.json`    | register, create_session, stop, deregister, bridge_link |
| **environment_secret**   | Registration response            | poll                                  |
| **session_ingress_token**| Work item secret (base64)        | ack, heartbeat                        |
| **worker_jwt**           | Bridge link response             | PUT /worker, SSE stream, POST /worker/events, POST /worker/events/delivery |

### Header Sets

**Full headers** (register, poll, ack, heartbeat, stop):

```http
Authorization: Bearer <token>
Content-Type: application/json
anthropic-version: 2023-06-01
anthropic-beta: ccr-byoc-2025-07-29,environments-2025-11-01
x-environment-runner-version: 2.1.79
x-organization-uuid: <org_uuid>
```

**Simple headers** (bridge_link, worker endpoints):

```http
Authorization: Bearer <token>
Content-Type: application/json
anthropic-version: 2023-06-01
```

---

## Complete Flow

```
1. Load credentials
2. Register environment          POST /v1/environments/bridge
3. Create session (optional)     POST /v1/sessions
4. Poll for work                 GET  /v1/environments/{env}/work/poll
5. Decode work secret            base64 → JSON
6. Acknowledge work              POST /v1/environments/{env}/work/{id}/ack
7. Bridge link                   POST /v1/code/sessions/{cse}/bridge
8. Register worker               PUT  {session_url}/worker
9. Connect SSE stream            GET  {session_url}/worker/events/stream
10. Start heartbeat loop         POST /v1/environments/{env}/work/{id}/heartbeat
11. Event loop:
    a. Receive SSE event
    b. Report delivery (received + processed) — BEFORE handling
    c. Handle control_request → send control_response
    d. Handle user message → processing → send response → idle
12. Stop work                    POST /v1/environments/{env}/work/{id}/stop
13. Archive session              POST /v1/sessions/{session}/archive
14. Deregister                   DELETE /v1/environments/bridge/{env}
```

---

## Step 1: Register Environment

Tells the API "I am a machine that can run sessions."

```
POST https://api.anthropic.com/v1/environments/bridge
Authorization: Bearer <oauth_access_token>
Content-Type: application/json
anthropic-version: 2023-06-01
anthropic-beta: ccr-byoc-2025-07-29,environments-2025-11-01
x-environment-runner-version: 2.1.79
x-organization-uuid: <org_uuid>

{
  "machine_name": "my-machine",
  "directory": "/home/user/project",
  "max_sessions": 1,
  "metadata": { "worker_type": "echo-server" }
}
```

**Response 200:**

```json
{
  "environment_id": "env_01GETf6Qo3peWmsMahnm3Bq3",
  "environment_secret": "env_secret_abc123..."
}
```

Save `environment_id` and `environment_secret` — used in all subsequent calls.

---

## Step 2: Create Session (Optional)

Creates a session that appears on claude.ai/code. If you skip this, you can
wait for someone to create a session via the web UI that targets your environment.

```
POST https://api.anthropic.com/v1/sessions
Authorization: Bearer <oauth_access_token>
Content-Type: application/json
anthropic-version: 2023-06-01
anthropic-beta: ccr-byoc-2025-07-29

{
  "title": "echo-my-machine-20260320-070000",
  "events": [],
  "session_context": {
    "sources": [],
    "outcomes": []
  },
  "environment_id": "env_01GETf6Qo3peWmsMahnm3Bq3",
  "source": "remote-control"
}
```

**Response 200:**

```json
{
  "id": "session_01RqAEEceRYidafhXsMCU39o",
  "title": "echo-my-machine-20260320-070000",
  "session_status": "pending",
  "connection_status": "disconnected",
  "environment_id": "env_01GETf6Qo3peWmsMahnm3Bq3",
  "created_at": "2026-03-20T07:00:00.000Z",
  ...
}
```

The session immediately generates a work item for polling.

---

## Step 3: Poll for Work

Long-polls until a session needs to be handled.

```
GET https://api.anthropic.com/v1/environments/env_01.../work/poll
Authorization: Bearer <environment_secret>
Content-Type: application/json
anthropic-version: 2023-06-01
anthropic-beta: ccr-byoc-2025-07-29,environments-2025-11-01
x-environment-runner-version: 2.1.79
x-organization-uuid: <org_uuid>
```

**Response 200** (work available):

```json
{
  "id": "cse_01RqAEEceRYidafhXsMCU39o",
  "type": "work",
  "state": "queued",
  "secret": "eyJ2ZXJzaW9uIjoxLCJzZXNzaW9uX2luZ3Jlc3NfdG9rZW4iOi...",
  "data": {
    "id": "cse_01RqAEEceRYidafhXsMCU39o",
    "type": "session"
  }
}
```

**Response 200** (no work / timeout): empty body

### ID Mapping

- Work item `id`: `cse_XXXX` (code session ID)
- `data.id`: also `cse_XXXX` — used as the session identifier throughout
- The `session_` prefix form (`session_XXXX`) is used by the `/v1/sessions` API but not by the worker protocol

### Decoding the Secret

The `secret` field is **base64-encoded JSON**:

```json
{
  "version": 1,
  "session_ingress_token": "sit_abc123...",
  "api_base_url": "https://api.anthropic.com",
  "sources": [],
  "auth": [
    { "type": "oauth", "token": "eyJhb..." }
  ],
  "claude_code_args": {},
  "environment_variables": {}
}
```

The `session_ingress_token` is used for ack and heartbeat auth.

### Data Types

| `data.type`    | Meaning                              |
|----------------|--------------------------------------|
| `"session"`    | A user session to handle             |
| `"healthcheck"`| Server health check — skip silently  |

---

## Step 4: Acknowledge Work

Must be called after receiving a work item, before starting work.

```
POST https://api.anthropic.com/v1/environments/env_01.../work/cse_01.../ack
Authorization: Bearer <session_ingress_token>
Content-Type: application/json
anthropic-version: 2023-06-01
anthropic-beta: ccr-byoc-2025-07-29,environments-2025-11-01
x-environment-runner-version: 2.1.79
x-organization-uuid: <org_uuid>

{}
```

**Response 200:** `{}`

---

## Step 5: Bridge Link

Fetches a short-lived JWT (`worker_jwt`) used for all worker communication.

```
POST https://api.anthropic.com/v1/code/sessions/cse_01.../bridge
Authorization: Bearer <oauth_access_token>
Content-Type: application/json
anthropic-version: 2023-06-01

{}
```

**Response 200:**

```json
{
  "worker_jwt": "eyJhb...",
  "api_base_url": "https://api.anthropic.com",
  "expires_in": 14400,
  "worker_epoch": 1
}
```

- `worker_jwt` expires in `expires_in` seconds (4 hours)
- `worker_epoch` is an integer used in all subsequent worker calls
- The **session URL** is constructed as: `{api_base_url}/v1/code/sessions/{cse_id}`

---

## Step 6: Register Worker

Tells the server a worker is ready. **Required before events will flow on the SSE stream.**

```
PUT https://api.anthropic.com/v1/code/sessions/cse_01.../worker
Authorization: Bearer <worker_jwt>
Content-Type: application/json
anthropic-version: 2023-06-01

{
  "worker_status": "idle",
  "worker_epoch": 1
}
```

**Response 200:** `{}`

### Worker States

| State          | When to set                                    |
|----------------|------------------------------------------------|
| `"idle"`       | Ready for work, or just finished processing    |
| `"processing"` | Actively handling a user message               |

Transition: `idle → processing → idle → processing → ...`

Update status with the same `PUT /worker` endpoint, changing `worker_status`.

---

## Step 7: Connect SSE Stream

Open a long-lived GET connection to receive events from the client (user messages, control requests).

```
GET https://api.anthropic.com/v1/code/sessions/cse_01.../worker/events/stream
Authorization: Bearer <worker_jwt>
Accept: text/event-stream
anthropic-version: 2023-06-01
```

The response is a standard **Server-Sent Events** stream:

```
:keepalive

event: client_event
id: 1
data: {"event_id":"8c08bc23-...", "sequence_num":"1", "event_type":"control_request", "source":"client", "payload":{"request":{"subtype":"initialize"}, "request_id":"abc123", "type":"control_request", "uuid":"8c08bc23-..."}, "created_at":"2026-03-20T06:58:35.701Z"}

event: client_event
id: 2
data: {"event_id":"a6a850a2-...", "sequence_num":"2", "event_type":"control_request", "source":"client", "payload":{"request":{"model":"claude-opus-4-6","subtype":"set_model"}, "request_id":"set-model-123", "type":"control_request", "uuid":"a6a850a2-..."}, "created_at":"2026-03-20T06:58:36.236Z"}

event: client_event
id: 3
data: {"event_id":"533e3ab8-...", "sequence_num":"3", "event_type":"user", "source":"client", "payload":{"message":{"content":"hello world","role":"user"}, "parent_tool_use_id":null, "session_id":"session_01...", "type":"user", "uuid":"533e3ab8-..."}, "created_at":"2026-03-20T06:58:36.695Z"}

:keepalive

```

### SSE Envelope Format

Each `data:` line is a JSON object:

```json
{
  "event_id": "uuid",
  "sequence_num": "N",
  "event_type": "user | control_request | ...",
  "source": "client",
  "payload": { ... },
  "created_at": "ISO8601"
}
```

The `payload` field contains the actual event. The `event_id` is needed for
delivery acknowledgment.

### Keepalive

The server sends `:keepalive` comment lines periodically (~15s). These are SSE
comments (start with `:`) and carry no data.

### Reconnection

On reconnect, pass `?from_sequence_num=N` query parameter and
`Last-Event-ID: N` header to resume from the last seen sequence number.

---

## Step 8: Report Delivery

After receiving each SSE event, report both `"received"` and `"processed"`
**immediately, before any handling logic**:

```
POST https://api.anthropic.com/v1/code/sessions/cse_01.../worker/events/delivery
Authorization: Bearer <worker_jwt>
Content-Type: application/json
anthropic-version: 2023-06-01

{
  "worker_epoch": 1,
  "updates": [
    { "event_id": "8c08bc23-...", "status": "received" }
  ]
}
```

Then immediately:

```json
{
  "worker_epoch": 1,
  "updates": [
    { "event_id": "8c08bc23-...", "status": "processed" }
  ]
}
```

**Response 200:** `{}`

### Critical: Delivery Reports Gate WebSocket Delivery

The server **holds back WebSocket delivery** of events to the web UI until the
worker reports `"processed"`. If the worker handles the event (sends response
events) before reporting `"processed"`, the response events will arrive on the
WebSocket before the original event, causing **out-of-order rendering** and a
**stuck "Thinking..." indicator** in the web UI.

**Always report `"received"` and `"processed"` before any event handling.**

---

## Step 9: Handle Events

### Control Requests

The web UI sends `control_request` events for initialization and configuration.
Respond with a `control_response` via POST /worker/events.

**Initialize** (first event on session connect):

```json
// Inbound payload
{
  "type": "control_request",
  "request": { "subtype": "initialize" },
  "request_id": "abc123",
  "uuid": "..."
}
```

```json
// Response to send
{
  "type": "control_response",
  "response": {
    "subtype": "success",
    "request_id": "abc123",
    "response": {
      "commands": [],
      "output_style": "normal",
      "available_output_styles": ["normal"],
      "models": [],
      "account": {},
      "pid": 12345
    }
  }
}
```

**Set Model:**

```json
// Inbound payload
{
  "type": "control_request",
  "request": { "model": "claude-opus-4-6", "subtype": "set_model" },
  "request_id": "set-model-123",
  "uuid": "..."
}
```

```json
// Response
{
  "type": "control_response",
  "response": {
    "subtype": "success",
    "request_id": "set-model-123"
  }
}
```

### User Messages

```json
// Inbound payload
{
  "type": "user",
  "message": { "content": "hello world", "role": "user" },
  "session_id": "session_01...",
  "uuid": "..."
}
```

Note: `message.content` can be a plain string OR an array of content blocks:
```json
[{ "type": "text", "text": "hello world" }]
```

---

## Step 10: Send Events (Worker → Client)

```
POST https://api.anthropic.com/v1/code/sessions/cse_01.../worker/events
Authorization: Bearer <worker_jwt>
Content-Type: application/json
anthropic-version: 2023-06-01

{
  "worker_epoch": 1,
  "events": [
    {
      "event_type": "assistant",
      "payload": {
        "uuid": "new-uuid-here",
        "type": "assistant",
        "message": {
          "role": "assistant",
          "content": [{ "type": "text", "text": "echo: hello world" }]
        }
      }
    },
    {
      "event_type": "result",
      "payload": {
        "uuid": "another-uuid",
        "type": "result",
        "subtype": "success",
        "result": "echo: hello world"
      }
    }
  ]
}
```

**Response 200:** `{}`

### Event Envelope Format (Outbound)

Each event in the `events` array must have:

```json
{
  "event_type": "<type>",
  "payload": {
    "uuid": "<uuid v4>",
    "type": "<type>",
    ...event fields...
  }
}
```

The `uuid` field is **required** in the payload. Generate a UUID v4 for each event.

### Common Outbound Event Types

**Assistant text:**
```json
{
  "event_type": "assistant",
  "payload": {
    "uuid": "...",
    "type": "assistant",
    "session_id": "session_01...",
    "parent_tool_use_id": null,
    "message": {
      "role": "assistant",
      "type": "message",
      "id": "msg_...",
      "model": "claude-opus-4-6",
      "content": [{ "type": "text", "text": "response text here" }],
      "stop_reason": "end_turn",
      "stop_sequence": null,
      "usage": {
        "input_tokens": 0,
        "output_tokens": 0
      }
    }
  }
}
```

**Result (turn complete):**
```json
{
  "event_type": "result",
  "payload": {
    "uuid": "...",
    "type": "result",
    "session_id": "session_01...",
    "subtype": "success",
    "is_error": false,
    "result": "",
    "stop_reason": null,
    "duration_ms": 0,
    "duration_api_ms": 0,
    "total_cost_usd": 0,
    "num_turns": 0,
    "usage": {
      "input_tokens": 0,
      "output_tokens": 0
    }
  }
}
```

**Control response (initialize):**
```json
{
  "event_type": "control_response",
  "payload": {
    "uuid": "...",
    "type": "control_response",
    "session_id": "session_01...",
    "response": {
      "request_id": "<from the request>",
      "subtype": "success",
      "response": {
        "account": {},
        "available_output_styles": ["normal"],
        "commands": [],
        "models": [],
        "output_style": "normal",
        "pid": 2
      }
    }
  }
}
```

**Control response (set_model):**
```json
{
  "event_type": "control_response",
  "payload": {
    "uuid": "...",
    "type": "control_response",
    "session_id": "session_01...",
    "response": {
      "request_id": "set-model-...",
      "subtype": "success",
      "response": {
        "model": "claude-opus-4-6"
      }
    }
  }
}
```

### Session ID in Events

Event payloads must use the `session_` prefixed ID (e.g. `session_01...`), not
the `cse_` prefixed ID. The `cse_` prefix is only used in API endpoint URLs.

---

## Step 11: Heartbeat

Send every ~15 seconds while processing a work item.

```
POST https://api.anthropic.com/v1/environments/env_01.../work/cse_01.../heartbeat
Authorization: Bearer <session_ingress_token>
Content-Type: application/json
anthropic-version: 2023-06-01
anthropic-beta: ccr-byoc-2025-07-29,environments-2025-11-01
x-environment-runner-version: 2.1.79
x-organization-uuid: <org_uuid>

{}
```

**Response 200:**

```json
{
  "lease_extended": true,
  "state": "active"
}
```

If `lease_extended` is `false` or `state` is not `"active"`, the server is
requesting you stop.

---

## Step 12: Stop Work

Called when the worker is done with a session (user quit, error, etc.).

```
POST https://api.anthropic.com/v1/environments/env_01.../work/cse_01.../stop
Authorization: Bearer <oauth_access_token>
Content-Type: application/json
anthropic-version: 2023-06-01
anthropic-beta: ccr-byoc-2025-07-29,environments-2025-11-01
x-environment-runner-version: 2.1.79
x-organization-uuid: <org_uuid>

{ "force": false }
```

**Response 200:** `{}`

---

## Step 13: Archive Session

Archives a session, removing it from the active session list in the web UI.
Call this before deregistering to clean up sessions created by the worker.

```
POST https://api.anthropic.com/v1/sessions/session_01.../archive
Authorization: Bearer <oauth_access_token>
Content-Type: application/json
anthropic-version: 2023-06-01
anthropic-beta: ccr-byoc-2025-07-29,environments-2025-11-01
x-environment-runner-version: 2.1.79
x-organization-uuid: <org_uuid>

{}
```

**Response 200:** `{}`

Note: Uses the `session_` prefixed ID (not `cse_`).

---

## Step 14: Deregister

Called on shutdown to remove the environment. Without this, the environment
remains registered and appears as a stale entry in the environments list.

```
DELETE https://api.anthropic.com/v1/environments/bridge/env_01...
Authorization: Bearer <oauth_access_token>
Content-Type: application/json
anthropic-version: 2023-06-01
anthropic-beta: ccr-byoc-2025-07-29,environments-2025-11-01
x-environment-runner-version: 2.1.79
x-organization-uuid: <org_uuid>
```

**Response 200:** `{}`

### Shutdown Sequence

On graceful shutdown (SIGINT/SIGTERM), the worker should:

1. Stop the SSE event loop
2. Stop work (`POST .../stop`)
3. Archive the session (`POST /v1/sessions/{session_id}/archive`)
4. Deregister the environment (`DELETE /v1/environments/bridge/{env_id}`)

If the worker crashes without deregistering, the environment will remain
registered but the server will eventually detect the stale heartbeat and
mark it as disconnected.

---

## Complete Message Handling Sequence

When the user sends "hello" in claude.ai/code:

```
 Browser                     API Server                   Worker
    │                            │                            │
    │ ──── user types "hello" ──►│                            │
    │                            │ ── SSE: control_request ──►│ (initialize)
    │                            │◄── POST delivery ack ──────│ (received+processed)
    │                            │◄── POST /worker/events ────│ (control_response)
    │                            │                            │
    │                            │ ── SSE: control_request ──►│ (set_model)
    │                            │◄── POST delivery ack ──────│ (received+processed)
    │                            │◄── POST /worker/events ────│ (control_response)
    │                            │                            │
    │                            │ ── SSE: user event ───────►│ ("hello")
    │                            │◄── POST delivery ack ──────│ (received+processed)
    │                            │◄── PUT /worker ────────────│ (processing)
    │                            │◄── POST /worker/events ────│ (assistant + result)
    │                            │◄── PUT /worker ────────────│ (idle)
    │                            │                            │
    │ ◄── response appears ──────│                            │
```

---

## Error Handling

| Error                        | Cause                                          | Fix                         |
|------------------------------|-------------------------------------------------|-----------------------------|
| Poll 403 "scope requirement" | Using OAuth token for poll                      | Use environment_secret      |
| Ack 401 "Invalid token type" | Using environment_secret for ack                | Use session_ingress_token   |
| Heartbeat 401                | Using environment_secret for heartbeat          | Use session_ingress_token   |
| Stop 403                     | Using environment_secret for stop               | Use OAuth token             |
| Bridge link 401              | Using environment_secret or wrong headers       | Use OAuth token + simple headers |
| Worker events 400 "event_type required" | Events not wrapped in envelope format | Wrap: `{event_type, payload}` |
| No SSE events arrive         | Missing `PUT /worker { idle }` init step        | Call register_worker first  |
| SSE keepalive blocks parser  | `:keepalive` comment not drained from buffer    | Drain comment-only blocks   |
| UI shows gray messages       | Missing delivery ack                            | Report received + processed |
| UI thinking pulse won't stop | Worker not set back to idle, or delivery report late | PUT /worker idle after response; report processed BEFORE handling |
| UI shows response before input | Delivery report sent after response events     | Report processed immediately, before sending any response events |
| Duplicate user messages      | Worker echoes user event via worker API          | Don't echo user events — server delivers them separately via sessions API |

---

## Timing

| Parameter              | Value       |
|------------------------|-------------|
| Poll timeout           | 30s         |
| Heartbeat interval     | 15s         |
| SSE keepalive interval | ~15s        |
| Worker JWT expiry      | 14400s (4h) |
| HTTP request timeout   | 10s         |

---

## Web UI Real-Time Architecture

Understanding the web UI's event delivery is essential for correct worker
implementation.

### Dual Channel: HTTP POST + WebSocket

The web UI uses two channels:

1. **HTTP POST** (`POST /v1/sessions/{session_id}/events`) — sends user messages
   and control requests to the server
2. **WebSocket** (`wss://claude.ai/v1/sessions/ws/{session_id}/subscribe`) —
   receives all events (user echoes, assistant responses, results, control
   responses) in real-time

### Event Delivery Paths

There are two distinct paths events take to reach the WebSocket:

| Path | Source | Speed | Examples |
|------|--------|-------|----------|
| **Sessions API path** | User HTTP POST → server → WebSocket | Slow (~1-1.5s) | User messages |
| **Worker API path** | Worker POST /worker/events → server → WebSocket | Fast (~50ms) | Assistant, result, control_response |

### Message Rendering Order

The web UI renders messages in **WebSocket arrival order** — it does NOT sort by
`created_at` or sequence number. Messages are stored in a Map keyed by UUID
(for deduplication) and displayed in insertion order.

### Thinking State

For remote sessions, the "Thinking..." indicator is controlled by
`isSessionRunning`, which checks if the **last event** (including result type)
is a `result` event. If so, the session is "not running" and the indicator
dismisses. The `result` event is critical for dismissing the thinking state.

### Event History

`GET /v1/sessions/{session_id}/events?limit=1000` returns events in
**chronological order** (oldest first). Response format:

```json
{
  "data": [ ...events... ],
  "first_id": "uuid-of-first",
  "last_id": "uuid-of-last",
  "has_more": false
}
```

The `last_id` is used as `from_event_id` when connecting the WebSocket to avoid
missing events during the handoff.

---

## Alternative: Direct Session Creation (No Poll)

The runner also supports creating sessions via `POST /v1/code/sessions`:

```
POST https://api.anthropic.com/v1/code/sessions
Authorization: Bearer <oauth_access_token>
Content-Type: application/json
anthropic-version: 2023-06-01

{
  "title": "my-session",
  "bridge": {}
}
```

This returns a `session.id` starting with `cse_` and the bridge link response
is obtained separately. This path does **not** generate a work item for polling —
the worker calls bridge_link directly and connects to the SSE stream without
going through the poll/ack flow. This is the "env-less" mode used by the runner
for self-created sessions.
