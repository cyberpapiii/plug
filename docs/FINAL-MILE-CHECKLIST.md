# Final Mile Checklist

Baseline: `main` @ `fcd6c3f`

This is the single finish-line tracker for the remaining work needed to call the current roadmap
effectively complete. Keep this list small and check items off as they land on `main`.

## Product Work

- [ ] Decide whether a manual refresh IPC command is warranted
  Source: [todos/052-pending-p3-oauth-auth-lifecycle-observability.md](./../todos/052-pending-p3-oauth-auth-lifecycle-observability.md)
  Note: if warranted, implement it; if not, close the todo with evidence and rationale.

- [ ] Decide whether fully live runtime reconfiguration is still in scope for “production-ready”
  Source: [CLAUDE.md](./../CLAUDE.md)
  Note: this is currently listed as incomplete, but it is not tracked as a concrete todo yet.

## Documentation And Release Hygiene

- [ ] Update the risk register to reflect only current remaining risks
  Source: [docs/RISKS.md](./RISKS.md)

- [ ] Reduce the research breadcrumb list to only still-open questions
  Source: [docs/RESEARCH-BREADCRUMBS.md](./RESEARCH-BREADCRUMBS.md)

## Tracking Discipline

- [ ] Keep `docs/PLAN.md` aligned with `main`
- [ ] Keep `docs/PROJECT-STATE-SNAPSHOT.md` aligned with `main`
- [ ] Keep `CLAUDE.md` aligned with `main`
- [ ] Close or rename todos as they are completed

## Recently Completed

- [x] Redirect URI alignment on refresh verified as a non-issue
  Source: [todos/054-complete-p3-redirect-uri-refresh-nonissue.md](./../todos/054-complete-p3-redirect-uri-refresh-nonissue.md)

- [x] Mock OAuth provider integration coverage
  Source: [todos/053-complete-p3-mock-oauth-provider-integration-tests.md](./../todos/053-complete-p3-mock-oauth-provider-integration-tests.md)

- [x] Distinct refresh-exchange observability signal
  Source: [todos/052-pending-p3-oauth-auth-lifecycle-observability.md](./../todos/052-pending-p3-oauth-auth-lifecycle-observability.md)
