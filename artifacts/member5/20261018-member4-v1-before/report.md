# Member 5 synthetic baseline

Status: pass. Profile: release.
Source commit: `9e4ced3c390467690c1ca5a0b577f2bda923995f`; local changes and source hashes are recorded in summary.json.

Medians in milliseconds. Body is nested within command wall time; do not add them.
Separate verify runs AFTER the experiment command and is not capture-time hashing.

| Workload | Mode | n | Copy | Persist | Command wall | Body (paced sleeps included) | Verify CLI |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| small | native | 3 | 0.439 | 19.834 | 19.215 | 0.171 | N/A |
| small | teamfs | 3 | 3.054 | 9.025 | 35.317 | 10.387 | N/A |
| small | experiment | 3 | 3.809 | 9.588 | 111.292 | 10.536 | 6.006 |
| many | native | 3 | 7.746 | 1507.946 | 23.879 | 3.996 | N/A |
| many | teamfs | 3 | 306.884 | 16.383 | 304.584 | 280.097 | N/A |
| many | experiment | 3 | 276.695 | 11.433 | 406.140 | 276.624 | 135.168 |
| large | native | 3 | 6.554 | 50.460 | 51.937 | 26.594 | N/A |
| large | teamfs | 3 | 21.328 | 45.827 | 103.754 | 79.141 | N/A |
| large | experiment | 3 | 19.806 | 45.323 | 201.322 | 72.658 | 32.841 |
| paced_many | native | 3 | 8.076 | 1417.427 | 4043.751 | 4016.252 | N/A |
| paced_many | teamfs | 3 | 324.696 | 15.153 | 4956.696 | 4927.571 | N/A |
| paced_many | experiment | 3 | 416.486 | 16.369 | 5187.727 | 5086.670 | 158.874 |
| paced_large | native | 3 | 8.477 | 48.953 | 4358.983 | 4320.109 | N/A |
| paced_large | teamfs | 3 | 25.941 | 66.135 | 4292.627 | 4260.392 | N/A |
| paced_large | experiment | 3 | 22.056 | 49.332 | 4611.093 | 4483.392 | 37.298 |
| unused_large | native | 3 | 6.317 | 22.753 | 24.567 | 0.209 | N/A |
| unused_large | teamfs | 3 | 24.627 | 51.374 | 44.131 | 14.532 | N/A |
| unused_large | experiment | 3 | 22.199 | 52.725 | 137.572 | 15.467 | 5.725 |

Scope and limitations:

- Synthetic sequential read plus SHA256 workloads, not a scientific case or universal filesystem ranking.
- Fresh mount/store per sample; rotating mode order. No OS cache drop. Prepared data and repeated reads include warm/uncontrolled caches.
- TeamFS uses default direct I/O, no file audit/trace/auto-sync; in-memory audit and runtime metrics remain enabled.
- Input copy and persistence are separate. Native persistence fsyncs copied files and directories; TeamFS uses sync CLI. Their durability mechanisms differ.
- Command wall includes subprocess startup. Experiment wall also includes snapshot commit, capture, complete-input hashing and report publication.
- Snapshot, capture hash and report export are NOT separately instrumented: null means unavailable, never zero. Verify CLI is a separate post-run recheck.
- Body timing includes read/hash loops and configured paced sleeps, and is contained in command wall. Active body subtracts observed sleeps, not FUSE blocking. Mount, archiving and cleanup are excluded.
- Paced workloads use fixed rounds and 0.5 second sleeps, not fixed-duration throughput. All modes have identical requested sleeps; sleep jitter remains measured.
- Checkpoint sequence includes the pre-start save; periodic count is sequence minus one. New reports must have at least two periodic saves for paced cases; old versions without checkpoints use null.
- Old/new comparisons include all member-3 changes, including snapshot fingerprint and stronger verify; they do not isolate a checkpoint on/off effect.
- Workload RSS is Linux ru_maxrss for the workload process only. Daemon peak is observed VmRSS sampled every 10 ms during command execution, may miss peaks; runner memory is not measured.
- Database/WAL/SHM values are logical file lengths, not physical device write traffic. Report bytes exclude workload outputs, which are recorded separately.
- Every successful output matches an independent deterministic oracle; experiment input hashes, classification, read bytes and unused-file exclusion are checked.
- No optimization claim or final integration acceptance: rerun after member 3/4 changes and with the final scientific case.
