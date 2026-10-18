# TeamFS 全套回归记录

- 结论：通过（`bash scripts/check.sh` 退出码 0）
- 源码提交：`f4501341762784c0322a85411b985a9ec24070ce`；分支 main
- 环境：Ubuntu 24.04.1 LTS，内核 6.6.114.1-microsoft-standard-WSL2，rustc 1.75.0 (82e1608df 2023-12-21)，libfuse 2.9.9，Python 3.12.3，uid 1000，debug 构建
- 时间：2026-10-18T11:12:28+0800

| 套件 | 退出码 | 通过 | 失败 | 未支持 | 环境跳过 | 耗时 s |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| unit | 0 | 32 | 0 | 0 | 0 | 0.59 |
| smoke | 0 | 13 | 0 | 0 | 0 | 2.14 |
| v2 | 0 | 15 | 0 | 0 | 0 | 4.11 |
| v3 | 0 | 14 | 0 | 0 | 0 | 11.81 |
| v4 | 0 | 10 | 0 | 0 | 0 | 4.69 |
| v5 | 0 | 8 | 0 | 0 | 0 | 4.64 |
| v6 | 0 | 39 | 0 | 7 | 0 | 157.76 |
| blocks | 0 | 9 | 0 | 0 | 0 | 8.76 |
| experiments | 0 | 12 | 0 | 0 | 0 | 5.81 |
| attribution | 0 | 9 | 0 | 1 | 0 | 11.42 |
| member3 | 0 | 10 | 0 | 0 | 0 | 12.64 |
| member4 | 0 | 6 | 0 | 0 | 0 | 1.79 |
| 合计 | | 177 | 0 | 8 | 0 | |

## 应用场景（Git、Vim、rsync 及实验模式工具）

- pass：v6 / direct / Git workflow
- pass：v6 / direct / rsync round trip
- pass：v6 / direct / repeated Vim saves
- pass：v6 / cached / Git workflow
- pass：v6 / cached / rsync round trip
- pass：v6 / cached / repeated Vim saves
- pass：attribution / ls, find, stat, head, wc, sha256sum, tar, rsync, git and vim as experiment commands

## 未支持与跳过项（单列，不计为通过）

- unsupported：v6 / direct / SQLite DELETE without explicit sync — Reproduced DELETE-mode durability boundary: committed 30 rows, recovered 0. Use explicit TeamFS sync or validated PERSIST mode.
- unsupported：v6 / direct / mmap then snapshot — default direct_io intentionally does not offer shared mmap
- unsupported：v6 / direct / hardlink capability — hardlink is explicitly unsupported, errno=1
- unsupported：v6 / direct / xattr capability — xattr is explicitly unsupported, errno=95
- unsupported：v6 / cached / SQLite DELETE without explicit sync — Reproduced DELETE-mode durability boundary: committed 30 rows, recovered 0. Use explicit TeamFS sync or validated PERSIST mode.
- unsupported：v6 / cached / hardlink capability — hardlink is explicitly unsupported, errno=1
- unsupported：v6 / cached / xattr capability — xattr is explicitly unsupported, errno=95
- unsupported：attribution / shared mmap of a pinned input — shared mmap of pinned inputs fails with ENODEV (direct I/O); the open is still recorded

Bounded mounted scenarios on this machine and build. Unsupported and environment-skip cases are listed separately and never counted as passes; this is not a POSIX, production or arbitrary-application guarantee.
