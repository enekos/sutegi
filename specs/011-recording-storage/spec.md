# Recording storage and playback

Support bounded-memory recording ingestion and playback without adding dependencies.

- Storage declares streaming-write and seekable-read capabilities. Unsupported operations fail before consuming bytes; existing buffered methods retain their contract.
- Filesystem writes use private, unique temporary siblings. Short/erroring inputs never replace the published object; temporary files are cleaned on every return path. Complete objects are synced then atomically renamed. Sidecar metadata is not a transactional pair; use immutable keys.
- Open objects own their file handle so key replacement cannot change an in-flight playback body.
- HTTP responses support authoritative reader lengths, bounded copying, no body reads on HEAD, and connection termination on truncated output.
- Web download helper supports single closed, open-ended and suffix byte ranges; 206 includes Content-Range, 416 includes the object length. Invalid/multiple ranges and unsupported If-Range validators fall back to a full response.
- S3/database adapters explicitly decline the new streaming operations. S3 signed GET/PUT URLs are the supported large-object path for those deployments.

Validation: 200 MiB input from a bounded source; interrupted and concurrent writes; replacement during an open read; range/body lengths; short output and HEAD semantics; existing storage/backend suites.
