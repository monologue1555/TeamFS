# Member 5 synthetic baseline

Status: pass. Profile: release.
Source commit: `276e68e37381a2c56c853ca7243859c340431213`; local changes and source hashes are recorded in summary.json.

Medians in milliseconds. Body is nested within command wall time; do not add them.
Separate verify runs AFTER the experiment command and is not capture-time hashing.

| Workload | Mode | n | Copy | Persist | Command wall | Body (paced sleeps included) | Verify CLI |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| small | native | 3 | 0.295 | 19.373 | 20.036 | 0.229 | N/A |
| small | teamfs | 3 | 5.329 | 11.179 | 50.570 | 17.536 | N/A |
| small | experiment | 3 | 4.894 | 11.531 | 120.717 | 14.991 | 6.608 |
| many | native | 3 | 7.943 | 1576.574 | 29.598 | 4.822 | N/A |
| many | teamfs | 3 | 367.564 | 17.151 | 416.632 | 387.264 | N/A |
| many | experiment | 3 | 338.764 | 19.835 | 546.883 | 426.003 | 195.443 |
| large | native | 3 | 6.264 | 29.291 | 60.328 | 34.049 | N/A |
| large | teamfs | 3 | 22.146 | 63.664 | 108.831 | 83.694 | N/A |
| large | experiment | 3 | 20.613 | 59.784 | 195.094 | 84.249 | 31.076 |
| paced_many | native | 3 | 10.037 | 1354.774 | 4044.123 | 4015.832 | N/A |
| paced_many | teamfs | 3 | 326.409 | 12.339 | 4961.022 | 4934.472 | N/A |
| paced_many | experiment | 3 | 353.082 | 14.680 | 5108.801 | 4998.677 | 142.923 |
| paced_large | native | 3 | 6.400 | 36.127 | 4152.139 | 4126.283 | N/A |
| paced_large | teamfs | 3 | 29.830 | 77.950 | 4572.805 | 4542.672 | N/A |
| paced_large | experiment | 3 | 30.880 | 92.341 | 4628.774 | 4505.996 | 37.694 |
| unused_large | native | 3 | 6.428 | 32.716 | 22.594 | 0.207 | N/A |
| unused_large | teamfs | 3 | 24.461 | 66.944 | 39.842 | 14.152 | N/A |
| unused_large | experiment | 3 | 23.708 | 60.766 | 153.997 | 13.707 | 3.747 |

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
