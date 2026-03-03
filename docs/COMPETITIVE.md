# Competitive Analysis

Every MCP multiplexer, router, gateway, and proxy we could find — analyzed for what they do well, what they do poorly, and what gap fanout fills.

---

## Landscape Summary

| Tier | Project | Stars | Language | Type |
|------|---------|-------|----------|------|
| 1 | IBM ContextForge | 3,359 | Python | AI Gateway + Registry |
| 1 | Bifrost | 2,670 | Go | LLM Gateway (MCP secondary) |
| 1 | mcp-proxy (sparfenyuk) | 2,296 | Python | Transport bridge |
| 1 | MetaMCP | 2,067 | TypeScript | Docker aggregator |
| 1 | AgentGateway | 1,851 | Rust | Agentic proxy (Linux Foundation) |
| 1 | mcptools | 1,502 | Go | CLI inspector |
| 1 | Docker MCP Gateway | 1,271 | Go | Container-based gateway |
| 2 | MCPJungle | 882 | Go | Self-hosted gateway |
| 2 | TBXark/mcp-proxy | 651 | Go | Simple aggregator |
| 2 | Microsoft MCP Gateway | 496 | C# | K8s-native proxy |
| 2 | Agentic Gateway Registry | 468 | Python | Enterprise SSO gateway |
| 2 | Lasso Security | 351 | Python | Security-first gateway |
| 2 | MCP Mesh | 336 | TypeScript | Control plane |
| 2 | adamwattis/mcp-proxy | 197 | TypeScript | Simple aggregator |
| 2 | Gate22 | 165 | TypeScript | IDE governance |
| 2 | mcpproxy-go | 144 | Go | Desktop proxy + BM25 |
| 2 | pluggedin | 122 | TypeScript | RAG + Memory |
| 3 | mcpx (lydakis) | 19 | Go | Shell-native |
| 3 | mcp.run | -- | Wasm | Wasm servlets |
| 3 | Envoy AI Gateway | -- | Go/C++ | Infra-grade proxy |
| 3 | hyper-mcp | ~2 | Rust | Wasm plugin MCP server |

---

## Tier 1 Deep Analysis

### IBM ContextForge (3,359 stars)

**What it is**: The most feature-complete MCP gateway. Federates MCP, A2A, and REST/gRPC into a unified endpoint. Plugin-based with 40+ plugins. Redis-backed for multi-cluster K8s.

**Strengths**: Broadest protocol coverage. TOON compression for token efficiency. OpenTelemetry. Admin UI.

**Weaknesses**: 843 open issues. Python (performance ceiling). Heavyweight — requires Redis, multiple services. Way too heavy for personal use.

**Lesson for fanout**: Study their TOON compression and plugin architecture patterns. Don't replicate their complexity.

### Bifrost (2,670 stars)

**What it is**: Primarily an LLM gateway, with MCP gateway as a secondary feature. Go, single binary, 11us overhead at 5k RPS.

**Strengths**: 54x faster P99 vs LiteLLM. Adaptive load balancing. Cluster mode. Semantic caching.

**Weaknesses**: MCP is a secondary concern. Enterprise features behind commercial tier.

**Lesson for fanout**: Their performance bar (11us overhead) is a reference point. We should target < 5ms tool call overhead.

### mcp-proxy / sparfenyuk (2,296 stars)

**What it is**: Transport bridge — converts stdio to Streamable HTTP and vice versa. Since v0.8.0, can proxy multiple stdio servers.

**Strengths**: Simple and focused. Solves one problem well. pip installable.

**Weaknesses**: Not a full multiplexer. No management UI. No resilience. No tool-level routing.

**Lesson for fanout**: This is the "too simple" end of the spectrum. Users want more than transport bridging.

### MetaMCP (2,067 stars)

**What it is**: Docker-based aggregator. Namespace-based grouping. Per-tool toggling. Rate limiting. OIDC/SSO.

**Strengths**: Namespace concept for selective tool exposure. One-click switching. Pre-warmed sessions. Middleware pipeline.

**Weaknesses**: Docker dependency. Complex setup. 2-4GB RAM minimum. Python/TypeScript, not performance-optimized.

**Lesson for fanout**: Study the namespace/group concept. Implement something similar but simpler — maybe just tags or groups in config.

### AgentGateway (1,851 stars)

**What it is**: Next-gen agentic proxy. Linux Foundation. Rust. MCP + A2A. K8s Gateway API support.

**Strengths**: Rust performance. RBAC. Multi-tenancy. Linux Foundation backing.

**Weaknesses**: 205 open issues. Enterprise/K8s focus. Rapidly evolving API. Overkill for personal use.

**Lesson for fanout**: Closest competitor in language (Rust). Study their architecture. Differentiate on simplicity — they serve enterprises, we serve individuals.

### Docker MCP Gateway (1,271 stars)

**What it is**: Docker CLI plugin. Runs MCP servers in isolated containers. 300+ verified images.

**Strengths**: Security (container isolation, signed images, SBOMs). "Dynamic MCP" — agents discover/add servers mid-conversation.

**Weaknesses**: Docker dependency. 50-200ms container latency. Not for non-Docker environments.

**Lesson for fanout**: "Dynamic MCP" (agents adding servers during conversation) is a compelling idea. Consider it for later phases.

---

## Key Competitors by Attribute

### By Language
- **Rust**: AgentGateway, hyper-mcp, mcp-proxy (various) — enterprise or early-stage
- **Go**: Bifrost, mcptools, MCPJungle, TBXark/mcp-proxy, Docker Gateway, mcpproxy-go
- **Python**: IBM ContextForge, mcp-proxy (sparfenyuk), Lasso, Agentic Gateway
- **TypeScript**: MetaMCP, MCP Mesh, Gate22, pluggedin, adamwattis/mcp-proxy
- **C#**: Microsoft MCP Gateway

### By Deployment Model
- **Single binary**: Bifrost, mcptools, TBXark/mcp-proxy, mcpproxy-go
- **Docker required**: MetaMCP, Docker Gateway, IBM ContextForge
- **K8s required**: Microsoft MCP Gateway, AgentGateway (preferred)
- **pip install**: mcp-proxy (sparfenyuk), Lasso

### By Target User
- **Enterprise/team**: IBM ContextForge, AgentGateway, Microsoft, Lasso, MCP Mesh, Agentic Gateway
- **Individual developer**: mcptools, TBXark/mcp-proxy, mcpproxy-go, pluggedin
- **Both**: MetaMCP, Bifrost, MCPJungle

---

## The Gap fanout Fills

No existing project combines ALL of these:

| Requirement | Closest Competitor | Their Gap |
|------------|-------------------|-----------|
| Single binary, zero deps | Bifrost, TBXark/mcp-proxy | Not MCP-focused, no TUI |
| Beautiful TUI | mcpproxy-go (tray icon) | Not a real TUI, limited monitoring |
| Client-aware tool filtering | Nobody | **Nobody does this** |
| Token-efficient serving | IBM ContextForge (TOON) | Python, heavyweight |
| Concurrent multi-client | MetaMCP | Docker, TypeScript |
| Portless (.localhost) | Nobody | **Nobody does this** |
| AI agent UX (--output json) | mcptools | Inspector, not multiplexer |
| Auto-import from clients | Nobody | **Nobody does this** |
| Rust performance | AgentGateway | K8s-focused, enterprise |
| Personal/desktop focus | mcpproxy-go | Go, early stage, no TUI |

**fanout's unique position**: The only single-binary, TUI-equipped, client-aware, token-efficient, Rust-based MCP multiplexer designed for individual power users.

---

## Patterns Worth Adopting

### From FastMCP 3.0: Provider + Transform

FastMCP's architecture is the most elegant abstraction we've seen:
- **Providers** unify how components (tools/resources/prompts) are sourced
- **Transforms** are middleware for the component pipeline (namespace, rename, filter, version)
- Two-level transforms: provider-level (one server) and server-level (aggregated)

This maps directly to fanout: each upstream server is a Provider. Tool prefixing, filtering, and limiting are Transforms.

### From mcpproxy-go: BM25 Tool Filtering

Uses BM25 (text ranking algorithm) to select the most relevant tools for a query. Claims 99% token reduction and 43% accuracy improvement in tool selection.

Worth studying for our catalog/search mode.

### From Docker MCP Gateway: Dynamic MCP

Agents can discover and add MCP servers mid-conversation. The gateway becomes a server registry, not just a proxy.

Interesting for later phases — `fanout server add` during an agent session.

### From Envoy AI Gateway: Token-Encoded Sessions

Session state encrypted into the session ID itself. Any instance can decode without external state. Enables horizontal scaling.

Not needed for single-node desktop use, but interesting if fanout ever goes multi-instance.

### From Lasso Security: Request Scanning

Real-time inspection for prompt injection, command injection, PII exposure. Plugin-based.

Worth considering for later phases — optional security scanning pipeline.

---

## Competitive Moat

What makes fanout defensible:

1. **Rust performance** — Can't be matched by Python/TypeScript competitors without rewriting
2. **Single binary UX** — brew install + one command is unbeatable for onboarding
3. **Client-aware intelligence** — Deep knowledge of every client's quirks is hard to replicate
4. **Token efficiency** — Architectural advantage from the ground up, not bolted on
5. **TUI quality** — A beautiful TUI creates emotional attachment (like lazygit)
6. **Simplicity** — Enterprise competitors can't simplify without losing their enterprise customers
