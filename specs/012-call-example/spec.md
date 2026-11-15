# Sutegi call example

Compose the framework layers into a single-node two-browser call. Media uses
native WebRTC and optional external TURN. Sutegi handles authenticated room
sessions, bounded/off-reactor signalling, reconnect generation fencing,
streaming recording upload and authorized range playback.

The server owns peer routing and recording keys. Admission requires a live
HttpOnly session and matching Origin. Rooms hold two peers; session state is
bounded and expires. Drain rejects new sessions, allows only previously
connected sessions to reconnect, and closes transport at the deadline.

The browser explicitly requests media, serializes SDP/ICE, bounds ICE queues
and reconnect attempts, releases tracks on leave, and records only by a user
action. Recording samples are time/size bounded. No external AI or media SDK
is introduced. Multi-node state, SFU, PSTN, AI voice logic, retention and
resumability remain application/deployment concerns documented in the README.

Verify two contexts exchanging decoded audio/video in direct and relay-only
modes, reconnects, stale signalling, bad auth/origin, room isolation, recording
upload/playback/seek and HEAD/206/416. Keep browser tooling outside Cargo.
