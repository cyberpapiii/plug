# Bug: `channels_list` takes 60+ seconds due to unnecessary full-list sorting and aggressive rate limiting

**Repository:** slack-mcp-server
**Severity:** High — makes `channels_list` unusable in real-time AI workflows
**Affects:** Any workspace with 200+ channels

## Summary

Calling the `channels_list` tool in a workspace with ~500 channels consistently takes 60+ seconds to return results, even when requesting only a single page of 100 items. Root cause analysis reveals three compounding code-level performance issues in the handler and provider layers — not network, API quota, or memory constraints.

## Environment

- slack-mcp-server: built from source, latest `main`
- Transport: stdio (`--transport stdio`)
- Workspace: ~236 users, ~501 channels (mix of public, private, IM, mpim)
- macOS 15.4, Apple Silicon
- Observed through both direct stdio and MCP multiplexer proxy

## Root Cause Analysis

### Issue 1: Full O(n log n) sort on every `channels_list` call (CRITICAL)

**File:** `pkg/handler/channels.go`, lines 258-313

```go
func paginateChannels(channels []provider.Channel, cursor string, limit int) ([]provider.Channel, string) {
    logger := zap.L()

    sort.Slice(channels, func(i, j int) bool {
        return channels[i].ID < channels[j].ID   // <-- sorts ALL channels every call
    })

    startIndex := 0
    if cursor != "" {
        // ... cursor decode, linear scan for start position
    }

    endIndex := startIndex + limit
    if endIndex > len(channels) {
        endIndex = len(channels)
    }
    paged := channels[startIndex:endIndex]
    // ...
}
```

`paginateChannels()` calls `sort.Slice()` over the **entire** filtered channel list on every invocation, even when only returning a small page. With 501 channels, that's ~4,500 comparisons per call. This sort is then followed by a **second** `sort.Slice()` for popularity sorting in the caller (`ChannelsHandler`, lines 170-174):

```go
sort.Slice(channelList, func(i, j int) bool {
    return channelList[i].MemberCount > channelList[j].MemberCount
})
```

So every `channels_list` call does **two full sorts** — first by ID for pagination, then by member count for display. The ID sort is entirely wasted when the popularity sort immediately follows.

**Suggested fix:** Maintain a pre-sorted cache (sorted by ID for cursor stability). Or, if cursor is empty and sort is "popularity", skip the ID sort entirely and do a single `sort.Slice` by member count, then slice. For cursor-based pagination, use `sort.Search` (binary search) instead of a linear scan after sorting.

### Issue 2: Aggressive rate limiter delays during cache refresh (CRITICAL)

**File:** `pkg/limiter/limits.go`, lines 22-24

```go
var (
    Tier2      = tier{t: 3 * time.Second, b: 3}
    Tier2boost = tier{t: 300 * time.Millisecond, b: 5}
    Tier3      = tier{t: 1200 * time.Millisecond, b: 4}
)
```

**File:** `pkg/provider/api.go`, lines 1005-1008

```go
func (ap *ApiProvider) GetChannelsType(ctx context.Context, channelType string) []Channel {
    // ...
    for {
        if err := ap.rateLimiter.Wait(ctx); err != nil {   // <-- 3-second wait per API page
            ap.logger.Error("Rate limiter wait failed", zap.Error(err))
            return nil
        }
        channels, nextcur, err = ap.client.GetConversationsContext(ctx, params)
        // ...
    }
}
```

When the cache is empty or expired, `refreshChannelsInternal()` fetches all 4 channel types (public, private, IM, mpim). Each type calls `GetChannelsType()`, which enters a pagination loop where **every API call** waits on the Tier2 rate limiter — 3 seconds per call.

With ~500 channels spread across 4 types, you get roughly 4-8 API pagination calls, each with a 3-second rate limit wait:

```
4 types x ~1.5 pages each x 3 seconds/page = 18 seconds minimum (rate limiting alone)
+ network latency for each call = 5-10 seconds
= 23-28 seconds just for cache refresh
```

The Tier2 rate of 1 request per 3 seconds (burst 3) is much more conservative than Slack's actual Tier 2 limit of ~20 requests/minute. The current implementation treats the rate limiter as a hard per-request delay rather than a sliding window.

**Suggested fix:**
- Use `Tier2boost` (300ms/5 burst) for cache refresh operations, which still stays well within Slack's actual API limits
- Or skip rate limiting entirely when reading from the in-memory cache (the rate limiter currently gates **all** calls, including cache hits)
- Consider using `Tier3` (1.2s/4 burst) as the default instead of `Tier2`

### Issue 3: Inefficient channel filtering with repeated slice reallocations (MEDIUM)

**File:** `pkg/handler/channels.go`, lines 212-256

```go
func filterChannelsByTypes(channels map[string]provider.Channel, types []string) []provider.Channel {
    var result []provider.Channel    // <-- starts at 0 capacity, grows via append
    typeSet := make(map[string]bool)
    for _, t := range types {
        typeSet[t] = true
    }

    for _, ch := range channels {    // <-- iterates ALL channels
        if typeSet["public_channel"] && !ch.IsPrivate && !ch.IsIM && !ch.IsMpIM {
            result = append(result, ch)
        }
        if typeSet["private_channel"] && ch.IsPrivate && !ch.IsIM && !ch.IsMpIM {
            result = append(result, ch)
        }
        if typeSet["im"] && ch.IsIM {
            result = append(result, ch)
        }
        if typeSet["mpim"] && ch.IsMpIM {
            result = append(result, ch)
        }
    }
    return result
}
```

The `result` slice starts at zero capacity and grows through repeated `append()` calls, causing multiple heap reallocations as it grows (Go doubles capacity each time). With 501 channels, you get ~9 reallocations (1, 2, 4, 8, 16, 32, 64, 128, 256, 512).

Also, each channel is checked against **all 4 type conditions** even though a channel can only match one type. An `if/else if` chain or `switch` with early `continue` would avoid unnecessary comparisons.

**Suggested fix:**
```go
result := make([]provider.Channel, 0, len(channels)) // pre-allocate
for _, ch := range channels {
    switch {
    case ch.IsIM && typeSet["im"]:
        result = append(result, ch)
    case ch.IsMpIM && typeSet["mpim"]:
        result = append(result, ch)
    case ch.IsPrivate && typeSet["private_channel"]:
        result = append(result, ch)
    case !ch.IsPrivate && !ch.IsIM && !ch.IsMpIM && typeSet["public_channel"]:
        result = append(result, ch)
    }
}
```

## Combined Impact

When `channels_list` is called:

| Phase | What happens | Time |
|-------|-------------|------|
| Cache refresh (if expired) | 4 channel types x ~1.5 API pages x 3s rate limit | ~18-28s |
| `filterChannelsByTypes()` | O(n) over 501 channels, multiple reallocations | ~50ms |
| `paginateChannels()` | Full `sort.Slice()` of 501 items | ~100ms |
| Popularity sort | Second full `sort.Slice()` of result set | ~50ms |
| CSV marshal | Marshal page to CSV | ~20ms |
| **Total** | | **20-30s (cached) to 60+s (cold)** |

The cache refresh is the dominant factor on cold starts, but even with a warm cache, the dual sorting adds unnecessary latency.

## Steps to Reproduce

```bash
# Start the server
./bin/slack-mcp-server --transport stdio

# Send MCP initialize + channels_list
echo '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"test","version":"1.0"}}}' | ...
echo '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"channels_list","arguments":{"channel_types":"public_channel","limit":1}}}' | ...

# Observe: even limit=1 takes 60+ seconds on first call
```

## Expected Behavior

`channels_list` with `limit=1` on a warm cache should return in <1 second. Even a cold cache refresh should complete in <10 seconds for a ~500-channel workspace.

## Proposed Solutions (Priority Order)

1. **Pre-sort the cache** — sort channels by ID once during cache build, not on every query
2. **Use `Tier2boost` for cache refresh** — 300ms intervals stay within Slack API limits while being 10x faster
3. **Pre-allocate filter result** — `make([]provider.Channel, 0, len(channels))`
4. **Skip ID sort when popularity sort follows** — the ID sort is wasted if the caller immediately re-sorts by member count
5. **Use binary search for cursor lookup** — `sort.Search` on a pre-sorted list is O(log n) vs current O(n)
