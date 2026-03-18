---
title: Doctor now points missing-config recovery to plug setup
date: 2026-03-18
category: integration-issues
status: completed
---

# Doctor now points missing-config recovery to plug setup

## Problem

`plug doctor` still suggested `plug init` when the config file was missing, even
though the supported onboarding flow is `plug setup`.

## Solution

- missing-config guidance now points to `plug setup`
- the doctor regression test now asserts the updated fix suggestion directly

## Why it matters

This was a small issue, but it directly affected the operator-truth goals of
the remediation plan: diagnostic output should point to real, supported actions.
