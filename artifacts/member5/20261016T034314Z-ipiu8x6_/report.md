# Member 5 synthetic baseline

Status: pass. Profile: release.
Source commit: `276e68e37381a2c56c853ca7243859c340431213`; local changes and source hashes are recorded in summary.json.

Medians in milliseconds. Body is nested within command wall time; do not add them.
Separate verify runs AFTER the experiment command and is not capture-time hashing.

| Workload | Mode | n | Copy | Persist | Command wall | Read/hash body | Verify CLI |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| small | native | 3 | 0.334 | 21.200 | 20.893 | 0.240 | N/A |
| small | teamfs | 3 | 3.689 | 9.961 | 40.436 | 13.417 | N/A |
| small | experiment | 3 | 3.720 | 9.095 | 114.221 | 11.322 | 3.614 |
| many | native | 3 | 8.652 | 1528.446 | 31.548 | 4.943 | N/A |
| many | teamfs | 3 | 403.204 | 21.937 | 410.605 | 384.061 | N/A |
| many | experiment | 3 | 463.613 | 17.977 | 545.199 | 441.681 | 199.470 |
| large | native | 3 | 7.295 | 27.475 | 60.120 | 36.009 | N/A |
| large | teamfs | 3 | 32.180 | 79.603 | 152.611 | 119.279 | N/A |
| large | experiment | 3 | 27.534 | 57.813 | 271.825 | 131.451 | 46.900 |

Scope and limitations:

- Synthetic sequential read plus SHA256 workloads, not a scientific case or universal filesystem ranking.
- Fresh mount/store per sample; rotating mode order. No OS cache drop. Prepared data and repeated reads include warm/uncontrolled caches.
- TeamFS uses default direct I/O, no file audit/trace/auto-sync; in-memory audit and runtime metrics remain enabled.
- Input copy and persistence are separate. Native persistence fsyncs copied files and directories; TeamFS uses sync CLI. Their durability mechanisms differ.
- Command wall includes subprocess startup. Experiment wall also includes snapshot commit, capture, complete-input hashing and report publication.
- Snapshot, capture hash and report export are NOT separately instrumented: null means unavailable, never zero. Verify CLI is a separate post-run recheck.
- Body timing covers read/hash loops only and is contained in command wall. Mount, evidence archiving and cleanup are excluded from those timings.
- Workload RSS is Linux ru_maxrss for the workload process only. Daemon peak is observed VmRSS sampled every 10 ms during command execution, may miss peaks; runner memory is not measured.
- Database/WAL/SHM values are logical file lengths, not physical device write traffic. Report bytes exclude workload outputs, which are recorded separately.
- Every successful output matches an independent deterministic oracle; experiment input hashes, classification, read bytes and unused-file exclusion are checked.
- No optimization claim or final integration acceptance: rerun after member 3/4 changes and with the final scientific case.
