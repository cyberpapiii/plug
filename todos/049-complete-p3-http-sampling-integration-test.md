---
status: complete
priority: p3
issue_id: "049"
tags: [code-review, testing]
dependencies: []
---

# Missing HTTP sampling reverse-request integration test

## Problem Statement

The integration tests cover:
- stdio elicitation round-trip
- stdio sampling round-trip
- HTTP elicitation round-trip

But there is no `test_http_sampling_reverse_request_round_trip`. The HTTP sampling path uses the same `HttpBridge::create_message` and `send_http_client_request` infrastructure as elicitation, but with a different timeout (60s vs None) and different response parsing. The code path is not identical.

Flagged by: code-simplicity-reviewer (observation), agent-native-reviewer (finding #10).

## Proposed Solutions

Add `test_http_sampling_reverse_request_round_trip` following the pattern of the HTTP elicitation test, with `--reverse-request sampling` and assertion on `reverse=sampling:model=mock-model`.

- Effort: Small (copy + adapt existing HTTP elicitation test)
- Risk: None

## Acceptance Criteria

- [ ] `test_http_sampling_reverse_request_round_trip` exists and passes
- [ ] Asserts on `reverse=sampling:model=mock-model` in response

## Work Log

| Date | Action | Learnings |
|------|--------|-----------|
| 2026-03-08 | Created from CE review | HTTP sampling path has different timeout than elicitation, worth separate test |
