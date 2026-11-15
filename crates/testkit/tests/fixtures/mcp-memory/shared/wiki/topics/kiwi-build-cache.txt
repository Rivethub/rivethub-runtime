---
title: Kiwi build cache
slug: kiwi-build-cache
aliases:
  - kiwi sccache
  - kiwi-build-cache bucket
tags:
  - kiwi
  - ci
last_verified: 2026-09-03T02:00:00Z
---

## Summary

Kiwi CI uses sccache backed by the shared kiwi-build-cache bucket with a 20 GB local tier per runner. Miss ratio about 0.11; pipeline about 14 minutes.

## Article

The Kiwi project moved its compiler cache off per-runner disks in September 2026. A baseline run showed a 0.83 miss ratio across 212 compile units; after the switch the ratio fell to 0.11.

## See also

- [[lighthouse-dashboard]]

## History

### 2026-09-02 — Cache moved to the shared bucket

- baseline miss ratio 0.83, wall time 39m52s
- after the switch: miss ratio 0.11, wall time 14m
- per-runner cache volumes retired

## Citations

| Date | Kind | Summary | Note |
|------|------|---------|------|
| 2026-09-03 | leaf | `b0000000-0000-4000-8000-000000000001` | miss ratio before/after |
| 2026-09-03 | root | `b0000000-0000-4000-8000-000000000003` | project overview |
