# Calls in Sutegi

Sutegi supplies bounded signalling, optional callback dispatch, lifecycle
coordination, and recording I/O. Browsers own WebRTC negotiation and media;
TURN is an external deployment service. The [call example](../examples/call/README.md)
composes those pieces into an authenticated, two-browser call.

| Layer | API | Application responsibility |
| --- | --- | --- |
| Transport | `Conn::try_send_*`, `WsConfig`, guarded `/__ws` | Handle `Full`/`Closed`, size queues for workload |
| Callbacks | `Ws::dispatch`, `Channels::dispatch` | Keep work bounded, check cancellation before publishing |
| Membership | `Socket::is_current`, generation-fenced pushes | Authorize each room; do not trust client peer identifiers |
| Lifecycle | `App::lifecycle`, `drain_timeout`, `/__ready` | Stop new sessions and identify authorized reconnects |
| Uploads | `App::upload`, `UploadLimits`, `RequestBody` | Authenticate headers, validate type, set byte/time limits |
| Storage | `Storage::capabilities`, `put_reader`, `open` | Choose supported backend, immutable keys, retention/quotas |
| Playback | `download`, `Response::with_reader` | Authorize before opening or signing a recording URL |

The stack deliberately keeps media protocol/runtime dependencies outside the
framework. The example is single-node with ephemeral room/session state;
shared routing and membership are required before scaling replicas.

See [Realtime](REALTIME.md) for queue/dispatch/lifecycle details,
[Streaming uploads](UPLOADS.md) for body/storage/range contracts, and the
[example verification script](../examples/call/verify.mjs) for browser checks.
