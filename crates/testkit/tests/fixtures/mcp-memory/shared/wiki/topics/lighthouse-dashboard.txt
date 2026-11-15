---
title: Lighthouse dashboard
slug: lighthouse-dashboard
aliases:
  - lighthouse p95
tags:
  - lighthouse
  - latency
last_verified: 2026-09-05T12:00:00Z
---

## Summary

Lighthouse dashboard p95 is about 180 ms. The per-tenant rollup depends on idx_rollups_tenant_bucket; alert lighthouse_p95_high fires above 400 ms for 10 minutes.

## Article

The dashboard reads a per-tenant rollup table. Its latency regressed when a migration dropped the rollup index.

## See also

- [[kiwi-build-cache]]

## History

### 2026-09-05 — Index regression fixed

- Tuesday migration dropped the (tenant_id, bucket_start) index
- index recreated concurrently; p95 back to 180 ms
- alert lighthouse_p95_high added

## Citations

| Date | Kind | Summary | Note |
|------|------|---------|------|
| 2026-09-05 | condensed | `b0000000-0000-4000-8000-000000000004` | index regression and alert |
