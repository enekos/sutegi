# 010 — Incremental upload requests

Split request head parsing from body consumption while sharing strict framing
validation. Keep existing buffered Request and service callers. Add an opt-in
route reader with limits and header authorization, per-route total/idle body
timeouts, sticky failures and connection-close responses. Reject unsupported
framing and require Content-Length. Do not raise ordinary JSON body limits.

Acceptance: 200 MiB generated from a fixed client buffer reaches a fixed-buffer
sink while Request.body stays empty. Auth/size rejection before sending body;
truncation and stalls cannot return success; drain interrupts a blocked upload.
Gate per constitution. No dependency changes.
